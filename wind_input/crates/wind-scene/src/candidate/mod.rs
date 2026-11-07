//! 候选窗场景：每帧把纯数据（[`CandidateSceneInput`]）构建成 View 树、排版、出命中矩形、绘制。
//!
//! 窗口侧（wind-ui 的 `CandidateWindow`）只负责收集输入、平台查询（DPI、显示器工作区）、定位与
//! 上屏；树怎么搭只看输入，不读窗口字段。跨帧状态（皮肤几何、分槽位交出的各块、装饰状态机）由
//! [`CandidateScene`] 持有；时间经 [`AnimClock`] 注入，文字经 [`TextBackend`] 注入。
//! 方案见 `docs/design/wind-scene-extraction.md` §2。

mod render;
mod skin_geom;
mod theme;

pub use render::{CandidateFrame, render_candidate};

pub use skin_geom::{AnimClock, AvoidMask, DecorGeom, PlaceAnchor, SkinGeom, SlotKind};
pub use theme::{EffectiveTheme, ThemeOverrides, effective_base_font_size, effective_theme};

use std::borrow::Cow;
use std::cell::RefCell;

use crate::text::{ColorRun, TextBackend, TextStyle};
use crate::view::{Align, Edges, Layout, LeftBar, Rect, View, ViewImage, ViewLayer};
use wind_theme::DEFAULT_ACCENT_BAR_HEIGHT_RATIO;
use wind_ui_types::{
    CandidateItem, HOVER_PAGE_NEXT as TAG_PAGE_NEXT, HOVER_PAGE_PREV as TAG_PAGE_PREV, SpanStyle,
    StyledText,
};

/// 注释节点没配文字色时的渲染层兜底（分段着色求色的 `body_fallback` 与之同源）。
const COMMENT_FALLBACK: [u8; 4] = wind_theme::fallback::COMMENT_TEXT;

/// 候选正文节点没配文字色时的渲染层兜底（模糊音提示求色的 `text_fallback` 与之同源）。
const TEXT_FALLBACK: [u8; 4] = [30, 30, 30, 255];

/// 候选的文字状态：选中优先于悬停（与 `eff_text` / `wind_theme::span_color` 同一口径）。
fn text_state(sel: bool, hov: bool) -> wind_theme::TextState {
    if sel {
        wind_theme::TextState::Selected
    } else if hov {
        wind_theme::TextState::Hover
    } else {
        wind_theme::TextState::Normal
    }
}

/// 右侧注释实际显示的内容。`merge_above`（旋转 / 直立态）不支持上方注释条（设计
/// candidate-comment-above-line.md §3.3），上段以 `\n` 接回下段之前，内容不丢；否则原样借用
/// `comment`——上方条关闭或该候选无上段时一律走借用，与改动前逐字节一致。
fn right_comment(c: &CandidateItem, merge_above: bool) -> Cow<'_, StyledText> {
    if !merge_above || c.comment_above.is_empty() {
        Cow::Borrowed(&c.comment)
    } else if c.comment.is_empty() {
        Cow::Borrowed(&c.comment_above)
    } else {
        Cow::Owned(StyledText::join(
            &[c.comment_above.clone(), c.comment.clone()],
            "\n",
        ))
    }
}

/// 换行可见符 U+21B5（`↵`）。取编辑器通用约定（VS Code 等显示换行即此符），
/// 而非 Control Pictures 区的 `␊`/`␤`——后者字形是小方框里塞 `LF` 字母，
/// 候选窗字号下几乎认不出，且部分中文字体不覆盖该区段。
const NEWLINE_GLYPH: char = '\u{21B5}';

/// 制表可见符 U+21E5（`⇥`）。与换行分用不同符号：两者在文本里的语义不同，
/// 混用一个符号等于告诉用户「这儿有个空白」却不说是哪种。
const TAB_GLYPH: char = '\u{21E5}';

/// 把候选文本里的换行/制表符替换为可见符号，**仅供显示**。
///
/// `Candidate::text` 本身不动——上屏走的仍是含真换行的原文。两者必须分开：
/// 显示要的是「看得见有个换行」，上屏要的是「真的换行」。
///
/// 不这么做的后果不是「看不见」而是「排版坏掉」：DirectWrite 的 `CreateTextLayout`
/// 把 `\n` 当硬换行，`TextRenderer::measure` 于是返回 N 倍行高，而 `view.rs` 的盒模型
/// 按内容高布局 → 该候选被撑成多行、整个候选窗变高且行高参差。
///
/// CRLF 只出一个符号（不是两个）——它在用户眼里就是一个换行。
fn visible_whitespace(s: &str) -> Cow<'_, str> {
    if !s.contains(['\n', '\r', '\t']) {
        return Cow::Borrowed(s); // 快路径：绝大多数候选无空白控制符，零分配
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push(NEWLINE_GLYPH);
            }
            '\n' => out.push(NEWLINE_GLYPH),
            '\t' => out.push(TAB_GLYPH),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// 带样式文字按区间边界切开的各段：`(起, 止, 该段样式)`，覆盖整串、无空段。
fn styled_segments(src: &StyledText) -> impl Iterator<Item = (usize, usize, SpanStyle)> + '_ {
    let mut cuts: Vec<usize> = src
        .spans()
        .iter()
        .flat_map(|sp| [sp.start as usize, sp.end as usize])
        .chain([0, src.len()])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    (1..cuts.len()).map(move |i| (cuts[i - 1], cuts[i], src.style_at(cuts[i - 1])))
}

/// 候选正文的显示副本（测宽 / 截断 / 绘制共用）：`text_styled` 非空用它（模糊音提示
/// 「苹(ping)果」），否则 `text`；空白控制符换成可见符号（[`visible_whitespace`]）。
/// 替换会改字节长度，带区间时逐段各自替换，区间才对得上。
fn cand_display(c: &CandidateItem) -> StyledText {
    if c.text_styled.is_empty() {
        return StyledText::from(visible_whitespace(&c.text).into_owned());
    }
    let src = &c.text_styled;
    if !src.as_str().contains(['\n', '\r', '\t']) {
        return src.clone();
    }
    let mut out = StyledText::new();
    for (a, b, style) in styled_segments(src) {
        out.push(&visible_whitespace(&src.as_str()[a..b]), &style);
    }
    out
}

/// 去掉全部模糊音提示段（`(ping)`），其余段样式不变。
fn without_pinyin_hint(src: &StyledText) -> StyledText {
    let mut out = StyledText::new();
    for (a, b, style) in styled_segments(src) {
        if style.role != Some(wind_theme::PINYIN_HINT_ROLE) {
            out.push(&src.as_str()[a..b], &style);
        }
    }
    out
}

/// 主题翻页字符 → 实际绘制的文本：未配（空串）时用内置箭头。
///
/// 仅空串算「未配」，空白字符不算——主题作者用 `prev_char = " "` 表达的是「这里留白」，
/// 不能被当成未配置而悄悄换回 ‹ ›。（编辑器那条路进不来这种值：它 trim 后空串即存 undefined，
/// 手写 toml 才配得出来。）
///
/// 只管「字符档画什么」，不管要不要走字符档——箭头图优先，见调用点。
fn arrow_char<'a>(configured: &'a str, builtin: &'a str) -> &'a str {
    if configured.is_empty() {
        builtin
    } else {
        configured
    }
}

/// 低频配置：随配置重载才变，与每帧内容分开存。
#[derive(Default)]
pub struct CandidateSceneOptions {
    /// 候选窗在光标上方时反转候选顺序（仅竖排生效，见 [`CandidateScene::above_layout`]）。
    /// 来自 ui.candidate.flip_when_above。
    pub flip_when_above: bool,
    /// 候选窗在光标上方时交换编码栏与候选栏位置（编码区整体沉底贴光标）。
    /// 与 flip_when_above 正交：可单独或叠加使用。来自 ui.candidate.swap_preedit_when_above。
    pub swap_preedit_when_above: bool,
    /// 翻页栏并入编码栏行、右对齐显示（竖排省一行）。仅"非嵌入编码"（有独立编码栏）时生效。
    /// 来自 ui.candidate.pager_in_preedit。
    pub pager_in_preedit: bool,
    /// 翻页栏显示覆盖（""跟随主题/"hide"/"auto"/"always"）。来自 ui.candidate.pager_bar_display。
    ///
    /// **横竖各一份**（`.0`＝横排、`.1`＝竖排）：协调器按 `ByLayout` 下发两档，这边按
    /// `vertical` 取。⚠️ 旋转态的 `vertical` 是 false ⇒ 落横排那份，与候选注释
    /// 模板的分档口径一致（见 `wind-config` 的 `comment_max_chars`）。
    pub pager_display: (String, String),
    /// 页码文字显示覆盖（""跟随主题/"show"/"hide"）。来自 ui.candidate.page_number_display。
    /// 横竖各一份，口径同 [`Self::pager_display`]。
    pub page_number_display: (String, String),
    /// 横排时窗口最小宽度，单位 dp（0=不限）。来自 ui.candidate.min_window_width_horizontal。
    pub min_window_width_horizontal: u32,
    /// 竖排时窗口最小宽度，单位 dp（0=不限）。来自 ui.candidate.min_window_width_vertical。
    pub min_window_width_vertical: u32,
    /// 横排时窗口最小高度，单位 dp（0=不限）。来自 ui.candidate.min_window_height_horizontal。
    pub min_window_height_horizontal: u32,
    /// 竖排时窗口最小高度，单位 dp（0=不限）。来自 ui.candidate.min_window_height_vertical。
    pub min_window_height_vertical: u32,
    /// 竖排最小行数，不足补透明占位行（0=不补）。来自 ui.candidate.min_rows。
    pub min_rows: u32,
}

/// 构建一帧候选树所需的全部输入。字段含义同窗口上的同名字段（见 wind-ui 的 `CandidateWindow`）。
pub struct CandidateSceneInput<'a> {
    // ── 每帧内容 ──
    pub candidates: &'a [CandidateItem],
    pub preedit: &'a str,
    /// `preedit` 内字节偏移（恒在字符边界）。
    pub preedit_caret: usize,
    pub mode_label: &'a str,
    pub selected: usize,
    /// 悬停项（页内下标），-1 = 无。
    pub hover: i32,
    pub page: usize,
    pub total_pages: usize,
    pub grid: Option<wind_ui_types::CandidateGrid>,
    // ── 排布 ──
    pub vertical: bool,
    pub rotated: bool,
    pub upright: bool,
    pub preedit_embedded: bool,
    /// 窗口落在光标上方（按上方专属布局搭，见 [`CandidateScene::above_layout`]）。
    pub above: bool,
    // ── 生效主题与尺度 ──
    pub theme: &'a wind_theme::Resolved,
    /// 生效的候选基准字号（逻辑像素）。
    pub base_font_logical: f32,
    /// DPI 缩放（主题几何为逻辑像素，渲染时乘此）。
    pub scale: f32,
    /// 候选窗所在显示器的工作区 `(宽, 高)`（设备像素）；None = 查不到，按兜底常量。
    pub screen_extent_px: Option<(u32, u32)>,
    pub text_renderer: &'a dyn TextBackend,
    pub opts: &'a CandidateSceneOptions,
}

/// 候选窗场景：持有跨帧状态，按 [`CandidateSceneInput`] 构建 View 树。
#[derive(Default)]
pub struct CandidateScene {
    /// 异形皮肤在当前 DPI / 字号下的几何；None = 本帧不启用皮肤。每帧渲染前重算。
    pub skin_geom: Option<skin_geom::SkinGeom>,
    /// 分槽位布局下 `build_tree`（`&self`）交出的各块，由 `skin_layout` 取走排版。
    pub(crate) free_parts: RefCell<Vec<(skin_geom::SlotKind, View)>>,
    /// 皮肤装饰的事件状态机，按当前变体的装饰下标一块一台（见 [`crate::skin_anim`]）。
    pub(crate) decor_anims: Vec<crate::skin_anim::Machine>,
    /// [`Self::decor_anims`] 对应的变体（竖排, 嵌入）；None = 待重建。
    pub decor_anims_key: Option<(bool, bool)>,
    /// 已报过的皮肤片段告警（每帧都重求几何，同一条只报一次；换主题清）。
    pub skin_warned: RefCell<std::collections::HashSet<String>>,
    /// 下一次换帧时刻（距动画时间轴起点）；None = 本帧没有在播的动画。隐藏时清空。
    pub anim_next: Option<std::time::Duration>,
}

/// 一帧排版结果：[`CandidateScene::layout`] 产出，`relayout_above` / `hits` / `paint` 消费。
pub struct SceneFrame {
    pub root: View,
    /// 软件阴影；None = 不画（皮肤自带外形、主题或用户关了阴影、调用方不画软件阴影）。
    pub shadow: Option<crate::view::SoftShadow>,
    /// 阴影外形的圆角（设备像素）：窗口 `border_radius`，没配按 8dp。
    pub shadow_radius: f32,
    /// 内容盒在缓冲里的外扩 `(左, 上, 右, 下)`（设备像素）：皮肤装饰外扩或阴影扩边。
    pub margins: (u32, u32, u32, u32),
    /// 根容器量出的自然宽（钳制前，设备像素）。
    pub natural_w: f32,
    /// 钳制后的内容盒尺寸（设备像素）。
    pub content_w: u32,
    pub content_h: u32,
    /// 屏幕安全上限真的把内容钳窄了：`(钳前宽, 扣掉扩边后的上限)`。正常为 None。
    pub safety_clamped: Option<(u32, u32)>,
}

impl SceneFrame {
    /// 缓冲尺寸 = 内容盒 + 四向扩边。
    pub fn size(&self) -> (u32, u32) {
        let (ml, mt, mr, mb) = self.margins;
        (self.content_w + ml + mr, self.content_h + mt + mb)
    }
}

impl CandidateSceneInput<'_> {
    /// 当前排版方向（`self.vertical`）下生效的窗口最小宽度（设备像素；0=不限）。
    ///
    /// ★ 按 `self.vertical`（用户配置的排布）取，不按 `list_vertical`（候选列表**物理**是否
    /// 竖排列——无候选时强制横排提示行）：提示态也要延续用户配置的排布方向，否则从
    /// 「只有模式徽标」到「出候选」的那一刻窗口仍会按另一套下限跳一下。
    ///
    /// 两个消费者共用本函数（`build_tree` 施加到根容器、[`CandidateScene::layout`] 对抗主题
    /// `behavior.vertical_max_width` 上限），避免两处各算一套而分叉。
    pub fn min_window_w_px(&self) -> u32 {
        let dp = if self.vertical {
            self.opts.min_window_width_vertical
        } else {
            self.opts.min_window_width_horizontal
        };
        CandidateScene::dp_to_px(dp, self.scale)
    }

    /// 当前排版方向下生效的窗口最小高度（设备像素；0=不限）。见 [`Self::min_window_w_px`]。
    pub fn min_window_h_px(&self) -> u32 {
        let dp = if self.vertical {
            self.opts.min_window_height_vertical
        } else {
            self.opts.min_window_height_horizontal
        };
        CandidateScene::dp_to_px(dp, self.scale)
    }

    /// 内容宽度的**屏幕安全上限**（设备 px，恒生效，不受排布方向/主题配置影响）。
    ///
    /// 与 `behavior.vertical_max_width`（主题的美观类上限，默认 0=不限、仅竖排生效）是两回事：
    /// 那个可以关，这个不能关——它是最后一道防线，防止异常长候选（比如超长拼音产生的候选）把
    /// 窗口撑出显示器边界，届时 `place_window`/`clamp_content_to_monitor` 就算把位置钳回屏内，
    /// 窗口本身也已经宽到放不下、必然探出另一侧。
    ///
    /// 有 [`Self::screen_extent_px`]（Windows：窗口侧查当前候选窗所在显示器的工作区，精确、
    /// 随屏幕/DPI 动态变化）就取它的宽。
    ///
    /// 没有（非 Windows：本地服务进程查不到系统显示器几何（真正的屏幕钳制在 `.app` 侧用
    /// `NSScreen.visibleFrame` 做，但那只管窗口**位置**、不管内容**宽度**——见
    /// [`crate::sys::clamp_content_to_monitor`] 文档））就退化为一个保守的固定安全值兜底：
    /// 不追求精确，只保证不会出现「宽到完全没法用」的窗口。
    /// TODO: 后续应通过 IPC 把 `.app` 侧的真实屏幕宽度传回来，替换这个兜底常量。
    pub fn screen_safety_max_width_px(&self) -> u32 {
        const FALLBACK_SAFETY_DP: u32 = 3000;
        match self.screen_extent_px {
            Some((w, _)) => w,
            None => CandidateScene::dp_to_px(FALLBACK_SAFETY_DP, self.scale),
        }
    }

    /// **局部排版横轴**在屏幕上对应那条边的可用长度（设备像素）。
    ///
    /// ★★ 旋转态下局部宽度 = 屏幕**高度**：候选文字沿屏幕纵向延伸，拿屏幕宽度当上限，
    /// 长候选会长到屏幕外——而这道钳制恒生效，正是为了防这件事。
    ///
    /// 与 [`Self::screen_safety_max_width_px`] 分成两个函数而不是加个参数：那三个调用点钳的是
    /// **窗口在屏幕上的宽度**（旋转态下同样是宽度，不该跟着翻），本函数给的是**文字预算的轴**。
    /// 同名同参只会让下一个人在两种语义间随手挑一个。
    pub fn local_text_extent_px(&self) -> u32 {
        if !self.rotated {
            return self.screen_safety_max_width_px();
        }
        const FALLBACK_SAFETY_DP: u32 = 2000;
        match self.screen_extent_px {
            Some((_, h)) => h,
            None => CandidateScene::dp_to_px(FALLBACK_SAFETY_DP, self.scale),
        }
    }

    /// 按目标像素宽度截断文本：超出则截到刚好放得下「前缀+…」的最长前缀，未超出原样返回。
    ///
    /// 只用于**显示层**（候选文字/编码栏）——截的是渲染用的显示副本，不改 `self.candidates`/
    /// `self.preedit` 本身，选中上屏、退格删字等仍走未截断的原文，不受影响。
    ///
    /// 按像素二分而非按字符数：汉字宽、字母窄，字符数相同不代表像素宽度相同（`max_chars` 那条
    /// 按字符数截断的路径是另一回事，见 `wind-config` 的 `truncate_display`）。
    ///
    /// `style` 必须由 [`CandidateScene::measure_style`] 按目标叶子的字族/字重构造，理由见该函数。
    ///
    /// ⚠️ `max_w <= 0`（预算被行内其它成员吃光）语义是**截到最短**（1 字 + …），不是「不截断」：
    /// 后者与预算耗尽的含义正好相反，会把溢出放大而非收敛。
    /// 候选文字的截断。直立态与其余形态**量的不是同一根轴**，故单开一层分派。
    ///
    /// ★ 直立态每个字是一格逆时针扶正的单元，它沿排布方向占的是**字的高度**（行高），
    /// 不是字的前进宽度。直接复用 [`Self::truncate_text_for_width`] 会按横向宽度估容量：
    /// 汉字两者接近（差 ~20%），拉丁差 2 倍以上（前进宽 ~0.5em、行高 ~1.2em）——
    /// 表现是「英文候选在对联模式下戳出屏幕外」，而中文候选看着完全正常。
    ///
    /// ⚠️ 不能把判据塞进 [`Self::truncate_text_for_width`] 本身：编码栏与模式徽标也调它，
    /// 而它们**没有**被切成格，量的仍是横向宽度。
    pub fn truncate_candidate_text(&self, text: &str, style: &TextStyle, max_w: f32) -> String {
        if !self.upright {
            return self.truncate_text_for_width(text, style, max_w);
        }
        let cells = crate::text::script::upright_cells(text);
        if cells.is_empty() {
            return String::new();
        }
        // 逐格量高度而不是「格数 × 行高」：回退字体的 line metrics 不同，行高会随内容变，
        // 而这里必须与 View 实际排出来的一致（预算按一种算、排版按另一种走 = 溢出）。
        let extent = |cs: &[&str]| -> f32 {
            cs.iter()
                .map(|c| self.text_renderer.measure(c, style).height)
                .sum()
        };
        const EXTENT_EPS: f32 = 0.5; // 与 truncate_text_for_width 的半像素容差同源
        if max_w > 0.0 && extent(&cells) <= max_w + EXTENT_EPS {
            return text.to_string();
        }
        // 放不下：末格换成省略号，从长到短找第一个装得下的。至少留一格，否则什么都看不见。
        let ell_h = self.text_renderer.measure("…", style).height;
        let mut keep = cells.len().saturating_sub(1);
        while keep > 1 && (max_w <= 0.0 || extent(&cells[..keep]) + ell_h > max_w + EXTENT_EPS) {
            keep -= 1;
        }
        let mut out: String = cells[..keep.max(1)].concat();
        out.push('…');
        out
    }

    /// 候选正文（带分段）的截断：放得下原样返回；放不下时若带模糊音提示，**整段去掉全部提示**
    /// 再按原逻辑截正文——提示要么完整、要么不在，不会截出 `(pi…`。截断走
    /// [`Self::truncate_candidate_text`]（结果恒为「前缀 + …」），区间随 `cut_with_mark` 一起裁。
    pub fn truncate_candidate_styled(
        &self,
        src: &StyledText,
        style: &TextStyle,
        max_w: f32,
    ) -> StyledText {
        let shown = self.truncate_candidate_text(src.as_str(), style, max_w);
        if shown == src.as_str() {
            return src.clone();
        }
        if src
            .spans()
            .iter()
            .any(|s| s.role == Some(wind_theme::PINYIN_HINT_ROLE))
        {
            return self.truncate_candidate_styled(&without_pinyin_hint(src), style, max_w);
        }
        src.cut_with_mark(shown.len() - '…'.len_utf8(), "…")
    }

    /// 带样式文本**逐行**按宽截断（`\n` 分行，每行各自 [`Self::truncate_text_for_width`]），
    /// 颜色区间随 `cut_with_mark` 一起裁。全部放得下返回 `None`（调用方沿用原文、不分配）。
    ///
    /// 逐行而非整段：竖排注释模板可以写 `\n` 排成多行，整段二分会在首行超长时把后面几行
    /// 整个裁掉。
    pub fn truncate_styled_lines(
        &self,
        src: &StyledText,
        style: &TextStyle,
        max_w: f32,
    ) -> Option<StyledText> {
        let text = src.as_str();
        let mut parts = Vec::new();
        let mut changed = false;
        let mut start = 0;
        for line in text.split('\n') {
            let end = start + line.len();
            let seg = src.slice(start, end);
            let shown = self.truncate_text_for_width(line, style, max_w);
            if shown == line {
                parts.push(seg);
            } else {
                changed = true;
                parts.push(seg.cut_with_mark(shown.len() - '…'.len_utf8(), "…"));
            }
            start = end + 1;
        }
        changed.then(|| StyledText::join(&parts, "\n"))
    }

    pub fn truncate_text_for_width(&self, text: &str, style: &TextStyle, max_w: f32) -> String {
        if text.is_empty() {
            return String::new();
        }
        // 半像素容差：不同后端的浮点连乘可能让「恰好等宽」冒出几个 ulp 的误差，
        // 卡在预算边界上的文字不该只因这点误差被多裁一个字——差半像素肉眼分不出来。
        const WIDTH_EPS: f32 = 0.5;
        if max_w > 0.0 && self.text_renderer.measure(text, style).width <= max_w + WIDTH_EPS {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        // 二分找刚好放得下「前 mid 字 + …」的最大 mid（单调：mid 越大越宽，可二分）。
        // max_w <= 0 时 hi=0，直接落到下面的「至少留一个字」。
        let (mut lo, mut hi) = (0usize, if max_w > 0.0 { chars.len() } else { 0 });
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            let probe: String = chars[..mid].iter().chain(['…'].iter()).collect();
            if self.text_renderer.measure(&probe, style).width <= max_w + WIDTH_EPS {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        // 连一个字符 + 省略号都放不下时至少保留一个字符，不然什么都看不见。
        let keep = lo.max(1).min(chars.len());
        chars[..keep].iter().chain(['…'].iter()).collect()
    }

    /// 当前排布该用的翻页栏覆盖档。
    pub fn pager_display_now(&self) -> &str {
        if self.vertical {
            &self.opts.pager_display.1
        } else {
            &self.opts.pager_display.0
        }
    }

    /// 当前排布该用的页码覆盖档。
    pub fn page_number_display_now(&self) -> &str {
        if self.vertical {
            &self.opts.page_number_display.1
        } else {
            &self.opts.page_number_display.0
        }
    }

    /// 是否显示翻页栏（覆盖优先；""跟随主题 behavior）。
    pub fn pager_visible(&self) -> bool {
        match self.pager_display_now() {
            "hide" => false,
            "always" => true,
            "auto" => self.total_pages > 1,
            _ => {
                // 跟随主题：hide_pager 隐藏；always_show_pager 总显示；否则 >1 页显示。
                let b = &self.theme.behavior;
                if b.hide_pager {
                    false
                } else if b.always_show_pager {
                    true
                } else {
                    self.total_pages > 1
                }
            }
        }
    }

    /// 翻页栏可见时是否显示页码文字（覆盖优先；""跟随主题 behavior.show_page_number）。
    pub fn page_number_visible(&self) -> bool {
        match self.page_number_display_now() {
            "show" => true,
            "hide" => false,
            _ => self.theme.behavior.show_page_number,
        }
    }

    /// 把 image ref 解析为可读绝对路径（委托共享 theme_assets）。
    pub fn asset_path(&self, reference: &str) -> Option<String> {
        crate::theme_assets::asset_path(self.theme, reference)
    }

    /// RvImage → 渲染用 ViewImage（委托共享 theme_assets）。
    pub fn rv_image(&self, im: Option<&wind_theme::RvImage>) -> Option<ViewImage> {
        crate::theme_assets::rv_image(self.theme, im, self.scale)
    }

    /// footer 翻页箭头图标（SVG + tint）。无 prev/next_image 时 None（回退文字箭头）。
    /// enabled→tint（如 ${accent}）；disabled→disabled_tint（如 ${text_hint}），缺则回退 tint。
    pub fn arrow_icon(&self, im: Option<&wind_theme::RvImage>, enabled: bool) -> Option<ViewImage> {
        let im = im?;
        let path = self.asset_path(&im.reference)?;
        let tint = if enabled {
            im.tint
        } else {
            im.disabled_tint.or(im.tint)
        };
        Some(ViewImage {
            path,
            // stretch：图标栅格化到其所在小节点尺寸（footer_fs，= 主题图标尺寸），
            // 该小节点再居中于更大的触摸盒内，从而"图标尺寸不变、操作区放大"。
            mode: "stretch".into(),
            slice: [0.0; 4],
            slice_repeat: [false; 2],
            opacity: 1.0,
            tint,
            place: None,
            scale: self.scale,
        })
    }

    /// 单个翻页箭头占的宽度：基线（字号 + 左右 padding）与实测文本宽取大。
    ///
    /// **两个消费点必须共用这一份**：pager 装配处给按钮定宽，`pager_node_w()` 给同一行的
    /// 编码/候选扣预留宽度。此前装配处 `fixed_w(arrow_w)`、预算处手写同一个公式，两份各自
    /// 正确；一旦按文本撑开（主题可以把 prev_char 写成多字，schema 不限长）还各写各的，
    /// 多字箭头下预算就比真实翻页栏窄，`pager_row_w` 的两个消费点都会算漏 —— 竖排内联
    /// 编码那条的注释早写明「否则超长编码会把翻页栏顶出窗口右缘」，横排 water-filling 则
    /// 是候选多分了宽度。
    ///
    /// 走图标档时实际宽恒为基线，这里仍按文本算 → 预算只会偏大几像素，不会算漏；为省这点
    /// 去重算一遍 `arrow_icon` 的资产解析不划算。
    /// `fs` 由调用处给出（字号是 `build_tree` 内按 base_fs + scale 算的，此处拿不到）。
    pub fn arrow_slot_w(
        &self,
        txt: &str,
        base_w: f32,
        fs: f32,
        footer: &wind_theme::RvNode,
    ) -> f32 {
        let style =
            CandidateScene::measure_style(fs, footer.font_weight, footer.font_family.as_deref());
        base_w.max(self.text_renderer.measure(txt, &style).width)
    }

    /// RvImage[] → ViewLayer[]（委托共享 theme_assets）。
    pub fn rv_layers(&self, layers: &[wind_theme::RvImage]) -> Vec<ViewLayer> {
        crate::theme_assets::rv_layers(self.theme, layers, self.scale)
    }

    /// RvGradient → 渲染用 ViewGradient（委托共享 theme_assets，弹窗类同一份）。
    pub fn rv_gradient(
        &self,
        g: Option<&wind_theme::RvGradient>,
    ) -> Option<crate::view::ViewGradient> {
        crate::theme_assets::rv_gradient(g)
    }

    /// 编码栏文本节点（含插入符）。
    ///
    /// 插入符走 `View::caret_at` 的覆盖层语义：文本作为**整串**一次整形，宽度与光标位置无关，
    /// 故移动光标不会让编码位移。曾把文本拆成「前半 + 竖线 + 后半」三节点，因
    /// `measure(a+b) != measure(a)+measure(b)`（字距在拆分边界丢失 + 每段各自舍入）、
    /// 且拆分点随光标而变，导致整串宽度抖动、字符跟着晃。
    /// `max_w`：显示层截断预算（设备 px）——超长编码（如超长拼音打字缓冲）按此宽度截断加省略
    /// 号，不影响 `self.preedit`/`self.preedit_caret` 本身。`caret_at` 自身会把插入符位置钳到
    /// 截断后字符串的合法边界内（见 `View::caret_at`），故截断字符串后原样传入光标位置即可，
    /// 不需要额外调整。
    /// 直立态：把一段文本拆成**逐格扶正**的单元；其余形态原样返回一个叶子。
    ///
    /// # 为什么注释、模式徽标、内联编码也要走它
    ///
    /// 旋转态是「**一切**都转」，自洽；直立态里若只有候选文字扶正、注释仍躺着，
    /// 同一列里就出现了两种阅读方向——真机反馈的原话是「看不懂」。
    /// ⇒ **凡是落在旋转包裹层内部的文本节点，一律逐格扶正**，拉丁也不例外。
    ///
    /// 唯一例外是序号：它整块扶正（「10」横着读，日文排版称 tate-chu-yoko）。
    /// 单位数下两种做法完全相同，多位数下横读才是纵排里数字的常规排法。
    ///
    /// ⚠️ 独立编码栏（非内联）**不**走它：那一栏在旋转包裹层**外面**，本就没转过。
    ///
    /// `leaf(片段, 该片段内的 caret 字节位, 该片段在整串里的字节偏移)` 由调用方构造——各处的
    /// 字号/字重/字族/颜色都不同，把它们全收成参数会得到一个七参数函数。不切格时偏移为 0。
    ///
    /// 偏移是给分段颜色用的：着色区间按整串的字节偏移给出，逐格切开后每格要取落在自己范围内的
    /// 那几段、平移到格内偏移（[`crate::text::ColorRun::slice`]）。
    pub fn upright_text(
        &self,
        text: &str,
        caret: Option<usize>,
        leaf: impl Fn(&str, Option<usize>, usize) -> View,
    ) -> View {
        let cells = if self.upright {
            crate::text::script::upright_cells(text)
        } else {
            Vec::new()
        };
        if cells.is_empty() {
            return leaf(text, caret, 0);
        }
        // 局部空间里是个 Row（左→右），经外层顺时针转完才是屏幕上的一列（上→下）。
        // 跨轴居中让宽窄不一的格（半角/全角混排）在列内对齐。
        let mut row = View::container(Layout::Row).cross(Align::Center);
        let mut off = 0usize;
        let last = cells.len() - 1;
        for (i, cell) in cells.iter().enumerate() {
            let end = off + cell.len();
            // caret 归它落进的那一格；落在整串末尾时归最后一格，否则末尾的插入符没处画。
            let local = caret
                .filter(|c| *c >= off && (*c < end || i == last))
                .map(|c| c - off);
            row = row.child(View::rotated_ccw(leaf(cell, local, off)));
            off = end;
        }
        row
    }

    /// `upright` = 本节点落在旋转包裹层内部（内联编码），需要逐格扶正。
    /// 独立编码栏传 `false`：它在包裹层外面，本就没转过。
    pub fn preedit_view(&self, fs: f32, max_w: f32, upright: bool) -> View {
        let v = &self.theme.views;
        let display = self.truncate_text_for_width(
            self.preedit,
            &CandidateScene::measure_style(
                fs,
                v.preedit_bar.font_weight,
                v.preedit_bar.font_family.as_deref(),
            ),
            max_w,
        );
        let color = v.preedit_bar.text_color.unwrap_or([100, 100, 100, 255]);
        let weight = v.preedit_bar.font_weight;
        let family = v.preedit_bar.font_family.clone();
        let caret_w = self.scale.max(1.0);
        // 直立态逐格切时 caret 由 `upright_text` 分派到它落进的那一格；转完是格间的一条
        // **横**线，正是纵排里插入符该有的样子。整块不切时行为与此前逐字节一致。
        let build = |seg: &str, caret: Option<usize>, _off: usize| {
            let mut leaf = View::leaf(seg.to_string(), color)
                .font_size(fs)
                .font_weight(weight)
                .font_family(family.clone());
            if let Some(c) = caret {
                // 竖线宽度随 DPI 缩放，但至少 1px（否则高分屏下会消失）。
                leaf = leaf.caret_at(c, caret_w);
            }
            leaf
        };
        if upright {
            self.upright_text(&display, Some(self.preedit_caret), build)
        } else {
            build(&display, Some(self.preedit_caret), 0)
        }
    }
}

impl CandidateScene {
    /// dp（逻辑像素）→ 设备像素，0 原样返回（0 是「不限」而非「0 像素」）。
    pub fn dp_to_px(dp: u32, scale: f32) -> u32 {
        if dp == 0 {
            return 0;
        }
        (dp as f32 * scale).ceil().max(1.0) as u32
    }

    /// 构造与 [`crate::view::View`] 叶子**完全同构**的测量样式。
    ///
    /// ★ 测量与渲染必须用同一套样式，否则宽度不是一个数：`View::text_style()` 取的是叶子上
    /// 挂的 `font_family`/`font_weight`，主题配了 `[text] font_family`、或候选选中加粗
    /// （`[item.selected].font_weight`）时，用 `TextStyle::new(size)`（默认字族 + 细体）
    /// 算出来的预算会系统性偏离实际排版宽度——预算按细体算、排版按粗体走，差值累积成窗口
    /// 右侧留白或右缘溢出。
    ///
    /// 归一化规则照抄 `View::font_weight`（`>0` 才生效）与 `View::font_family`（非空才生效），
    /// 少一条就还是两套样式。
    pub fn measure_style(fs: f32, weight: i32, family: Option<&str>) -> TextStyle<'_> {
        TextStyle {
            family: family.filter(|s| !s.trim().is_empty()),
            size: fs,
            weight: if weight > 0 { weight } else { 0 },
        }
    }

    /// 最大最小公平分配（water-filling）：把总额 `total` 分给 `demands`，需求低于均分份额的
    /// **原样满足**、把没用完的份额让给需求大的，需求大的平分剩余。返回与 `demands` 等长同序
    /// 的分配结果，`floor` 是每项最低保障。
    ///
    /// ★ 为什么横排必须用它而不是「先到先得」的贪心：每个候选除文字外还有一份**无条件计入**的
    /// 固定开销（item 内边距 / 序号 / 注释 / 候选间隙），贪心下第一个候选吃光预算后，后面每个
    /// 候选仍各自加上「固定开销 + 文字下限」，累加必然突破任何上限——真机实测 9 个候选溢出约
    /// 1100px。本函数保证：只要 `total >= 0` 且不触发 `floor`，`Σ result ≤ total` 恒成立，
    /// **与项数无关**，这正是贪心给不了的性质。
    ///
    /// `floor` 触发时（固定开销本身已超预算）总和可能超出 `total`——那是无解情形下的取舍：
    /// 宁可溢出被裁掉，也不能少显示候选（选中键位与协调器下发的候选列表绑定，少一个会让用户
    /// 按 8 选到错的词）。
    pub fn water_fill(demands: &[f32], total: f32, floor: f32) -> Vec<f32> {
        let n = demands.len();
        if n == 0 {
            return Vec::new();
        }
        // 按需求升序处理：先满足小需求，余量自然滚给后面的大需求。
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|a, b| demands[*a].total_cmp(&demands[*b]));
        let mut out = vec![0.0f32; n];
        let mut remaining = total;
        for (k, &i) in idx.iter().enumerate() {
            let share = remaining / (n - k) as f32;
            let give = demands[i].min(share).max(floor);
            out[i] = give;
            // 扣**实际给出**的量（而非未经 floor 的理论份额）：floor 生效时余额要跟着少，
            // 否则后面各项会按虚高的余额再算份额，超额层层放大。
            remaining = (remaining - give).max(0.0);
        }
        out
    }

    /// 两个「上方专属」开关的最终生效判定（纯函数）：返回 `(反转候选, 交换编码/候选带)`。
    ///
    /// `flip_when_above` **只对竖排成立**：竖排下候选沿光标方向纵向排开，"反转"= 让候选 1
    /// 落到离光标最近的一侧，语义明确；横排下候选是左右并列，反转只是把 1..n 变成 n..1，
    /// 读序被打乱且与窗口在上在下毫无关系（同 `inline_preedit_bottom` 对横排的处置）。
    /// `swap_preedit_when_above` 交换的是编码带与候选带的**上下**位置，横竖排都成立，不受此限。
    ///
    /// 抽成纯函数是为了让「重建视图树的触发条件」与「build_tree 内的实际派生」共用同一份
    /// 判据：否则横排开了 flip 时外层仍会每帧重建一棵与原树完全相同的树，白付 build+layout。
    pub fn above_layout(above: bool, vertical: bool, flip: bool, swap: bool) -> (bool, bool) {
        (above && vertical && flip, above && swap)
    }

    /// 窗口投影参数（共享 SoftShadow；读 RvViews 顶层 shadow_* 字段）。
    pub fn shadow_params(&self, inp: &CandidateSceneInput) -> Option<crate::view::SoftShadow> {
        // 皮肤自带外形与阴影，不叠软件阴影。
        if self.skin_geom.is_some() {
            return None;
        }
        let v = &inp.theme.views;
        crate::view::SoftShadow::build(
            v.shadow_offset_x,
            v.shadow_offset_y,
            v.shadow_blur,
            v.shadow_spread,
            v.shadow_spread_offset_x,
            v.shadow_spread_offset_y,
            v.shadow_color,
            inp.scale,
        )
    }

    /// 皮肤布局后的整张缓冲（内容盒 + 外扩）在位图上限内。
    fn skin_buffer_fits(root: &View, (l, t, r, b): (u32, u32, u32, u32)) -> bool {
        let (w, h) = root.measured_size();
        let side = |c: f32, a: u32, b: u32| (c.ceil().max(0.0) as u64) + a as u64 + b as u64;
        let (bw, bh) = (side(w, l, r), side(h, t, b));
        bw <= crate::skin::MAX_IMAGE_SIDE as u64
            && bh <= crate::skin::MAX_IMAGE_SIDE as u64
            && bw * bh <= crate::skin::MAX_IMAGE_PIXELS
    }

    /// 一帧的排版：刷新皮肤几何与装饰动画 → 搭树 → 皮肤布局或按阴影扩边布局 → 宽度钳制
    /// （竖排主题上限、屏幕安全上限、最小 40×24）→ 皮肤窗体按钳后尺寸重定位。
    ///
    /// `inp.above` 须为 false（上翻重排另走 [`Self::relayout_above`]）。`soft_shadow` = 调用方
    /// 画不画软件阴影：ext_presenter（macOS / Linux 外部宿主）由宿主出系统阴影，传 false，
    /// 扩边随之为 0。
    pub fn layout(
        &mut self,
        inp: &CandidateSceneInput,
        clock: skin_geom::AnimClock,
        soft_shadow: bool,
    ) -> SceneFrame {
        self.refresh_skin(inp, clock);
        let mut root = self.build_tree(inp);
        // 窗口投影：高斯软影四向扩边（与 Go shadowMargins 对齐），内容布局起点平移到 (ml, mt)，
        // 窗口显示位置再左上回移 (ml, mt) → 视觉锚点/命中坐标与无阴影时一致，阴影四面溢出。
        let shadow = if soft_shadow {
            self.shadow_params(inp)
        } else {
            None
        };
        let tr = inp.text_renderer;
        let margins = match self.skin_layout(&mut root, tr) {
            // 装饰偏移来自主题：外扩大到超出位图上限时整套回退普通窗口，别分配那张缓冲。
            Some(m) if Self::skin_buffer_fits(&root, m) => m,
            skinned => {
                if skinned.is_some() {
                    self.warn_skin_once(format!(
                        "skin: 装饰外扩 {skinned:?} 让窗口缓冲超出上限，回退普通窗口"
                    ));
                    self.skin_geom = None;
                    self.anim_next = None;
                    root = self.build_tree(inp);
                }
                let m = match &shadow {
                    Some(s) => s.margins(),
                    None => (0, 0, 0, 0),
                };
                root.layout(m.0 as f32, m.1 as f32, tr);
                m
            }
        };
        let (ml, _, mr, _) = margins;
        let (w_f, h_f) = root.measured_size();
        let mut content_w = (w_f.ceil() as u32).max(40);
        // 竖排最大宽度（behavior.vertical_max_width，单位 dp，默认 0=不限）：仅在用户/主题
        // 显式配置正值时生效。本 View 引擎不支持文本折行，超宽候选在窗口右缘裁切（draw_text
        // 按缓冲宽度裁剪，无省略号）——下面的屏幕安全上限是恒生效的另一道独立防线，两者不是
        // 同一回事。
        if inp.vertical && !inp.candidates.is_empty() {
            let vmax = inp.theme.behavior.vertical_max_width;
            if vmax > 0 {
                // 单位 dp，换算复用 dp_to_px（与 min_window_width_* 等字段同一套算法）。
                let vmax_px = Self::dp_to_px(vmax as u32, inp.scale).max(40);
                // 下限优先于上限：用户显式配的抗抖动宽度不该被主题的裁切上限压回去。
                // 见 [`CandidateSceneInput::min_window_w_px`]（未配 min 时该值为 0，行为不变）。
                content_w = content_w.min(vmax_px.max(inp.min_window_w_px()));
            }
        }
        // 屏幕安全上限：横竖排都生效，防止异常长候选（如超长拼音产生的候选）把窗口撑出
        // 显示器边界。与上面的主题上限是两条独立防线，见 screen_safety_max_width_px 文档。
        // 钳的对象须是「屏幕能放下的内容宽度」= 屏幕宽度 − 阴影扩边（窗口宽还要再加回
        // ml+mr）——直接拿 content_w 跟屏幕整宽比，加回扩边后窗口仍会探出屏幕（已实测命中：
        // natural=5195 safety_max=3840 clamped=3840，但 width=clamped+ml+mr 已经超了 3840）。
        let safety_max_w = inp.screen_safety_max_width_px().saturating_sub(ml + mr);
        let before_safety = content_w;
        content_w = content_w.min(safety_max_w.max(inp.min_window_w_px()));
        let content_h = (h_f.ceil() as u32).max(24);
        self.skin_fit_frame(content_w, content_h, tr);
        let shadow_radius = inp
            .theme
            .views
            .window
            .border_radius
            .map(|d| d.resolve(inp.scale, 0.0))
            .unwrap_or(8.0 * inp.scale);
        SceneFrame {
            root,
            shadow,
            shadow_radius,
            margins,
            natural_w: w_f,
            content_w,
            content_h,
            safety_clamped: (content_w < before_safety).then_some((before_safety, safety_max_w)),
        }
    }

    /// 定位定下落在光标上方后调用（`inp.above` = 是否上方）：有生效的「上方专属」布局且不是
    /// 分槽位皮肤时，按上方布局重建并在原扩边处重排（项数/尺寸/位置不变，仅排列翻转）。
    pub fn relayout_above(&self, inp: &CandidateSceneInput, f: &mut SceneFrame) {
        let (flip, swap) = Self::above_layout(
            inp.above,
            inp.vertical,
            inp.opts.flip_when_above,
            inp.opts.swap_preedit_when_above,
        );
        if (flip || swap) && !self.skin_is_free() {
            let (ml, mt, _, _) = f.margins;
            f.root = self.build_tree(inp);
            f.root.layout(ml as f32, mt as f32, inp.text_renderer);
        }
    }

    /// 本帧命中矩形（窗口缓冲坐标，内容起点在扩边处）：候选与翻页 + 分槽位皮肤各块。
    /// 须在 [`Self::relayout_above`] 之后取。
    pub fn hits(&self, f: &SceneFrame) -> Vec<(i32, Rect)> {
        let mut out = Vec::new();
        f.root.collect_hits(&mut out);
        if let Some(g) = &self.skin_geom {
            g.collect_part_hits(&mut out);
        }
        out
    }

    /// 画进 `buf`（BGRA 预乘，调用方已清零，宽高为 [`SceneFrame::size`]）：软件阴影 → 皮肤底层
    /// → 根 → 分槽位各块 → 皮肤上层。
    ///
    /// `split_pass` = 把 `hit = pass` 的装饰拆出去（只有 Windows 本地窗口路径置位，且它要先
    /// 确认穿透窗口建得出来）：主缓冲不画它们，另返回一张同尺寸的穿透层像素 + 是否放主窗口下面。
    /// 不拆（或皮肤里没有穿透装饰）时返回 None，穿透装饰照 block 画进主缓冲。
    ///
    /// 文字后端单独传而不取 `inp`：本地窗口路径画进窗口自己的缓冲，那份可变借用与整份输入
    /// 的借用冲突。须与排版时是同一个后端。
    pub fn paint(
        &self,
        f: &SceneFrame,
        buf: &mut [u8],
        tr: &dyn TextBackend,
        split_pass: bool,
    ) -> Option<(Vec<u8>, bool)> {
        let (width, height) = f.size();
        let (ml, mt, _, _) = f.margins;
        // 先画投影（在内容下方），再画内容覆盖其上。内容盒左上 = (ml, mt)。
        if let Some(s) = &f.shadow {
            let radius = f.shadow_radius;
            s.paint(
                buf,
                width,
                height,
                ml as f32,
                mt as f32,
                f.content_w as f32,
                f.content_h as f32,
                radius,
            );
        }
        let split = split_pass && self.skin_geom.as_ref().is_some_and(|g| g.has_pass());
        if let Some(g) = &self.skin_geom {
            g.paint(buf, width, height, false, split);
        }
        f.root.paint(buf, width, height, tr);
        if let Some(g) = &self.skin_geom {
            g.paint_parts(buf, width, height, tr);
        }
        if let Some(g) = &self.skin_geom {
            g.paint(buf, width, height, true, split);
        }
        match (&self.skin_geom, split) {
            (Some(g), true) => {
                let mut pass = vec![0u8; (width * height * 4) as usize];
                g.paint_pass(&mut pass, width, height);
                Some((pass, g.pass_below()))
            }
            _ => None,
        }
    }

    /// 按当前状态构建候选视图树（横向布局）。
    /// T3：从 RVNode 树（`Resolved.views`）取色/几何，颜色 None→兜底（与旧 ResolvedTheme 默认等值，零回归）。
    /// 构建候选 View 树。`above=true`（窗口上翻到光标上方）时按三个正交开关调整布局：
    /// 反转候选（flip_when_above，仅竖排）、交换编码/候选区（swap_preedit_when_above）、翻页栏并入编码栏
    /// （pager_in_preedit，与 above 无关的常驻行为）。任何情形下每项 tag/序号标签/选中判定仍用
    /// 原始索引 i，确保命中/选中映射不受排列变化影响。
    pub fn build_tree(&self, inp: &CandidateSceneInput) -> View {
        use wind_theme::rvnode::{RvEdges, RvNode};
        use wind_theme::schema::Dim;
        let above = inp.above;
        let t = &inp.theme;
        let v = &t.views;
        // 候选文字的字族：优先级（方案 > 用户 > 主题 > 默认）已在
        // `refresh_effective_theme` 折进生效主题，这里只取结果。
        //
        // ★ 只取一次、四个消费点（两处测量 + 候选叶子 + 占位行叶子）共用同一个值。
        // 各自去取的话必然漂移，而漂移的两种表现都不报错：测量与渲染不同源 ⇒ 预算按一种
        // 字体算、排版按另一种走，窗口右侧留白或右缘溢出；占位行漏掉 ⇒ 它与真实行行高不等
        // （本文件下方 `padded_rows_equal_real_rows_in_height` 已为同一件事钉过一次）。
        let text_family: Option<String> = v.text.font_family.clone();
        // above=true（窗口被上翻到光标上方）时，据两个正交开关派生上方专属行为：
        //   flip_cands = 反转候选项排列顺序（ui.candidate.flip_when_above，仅竖排有意义）
        //   swap_bands = 交换编码区↔候选区上下位置（ui.candidate.swap_preedit_when_above，编码沉底贴光标）
        // 二者可单独或叠加：叠加时候选块内部反转 + 编码沉底 = 候选1 紧挨编码栏。
        // 生效判据见 above_layout（与外层「要不要重建」共用同一份，避免两处各判一套）。
        let (flip_cands, swap_bands) = Self::above_layout(
            above,
            inp.vertical,
            inp.opts.flip_when_above,
            inp.opts.swap_preedit_when_above,
        );
        // 翻页栏并入**独立**编码栏行（右对齐）：需开关开启 + 有独立编码栏（非嵌入）+ 翻页栏本身可见。
        // 满足时翻页栏随编码区一同装配（swap 时自动跟随沉底）。
        // ⚠️ 这只是「并入编码所在行」的**两条落点之一**。内联编码（`candidate_inline`）那条是
        //    下方的 `pager_in_inline_row`（判据含 `list_vertical`，故没法与本条合并在此处算）。
        //    两条都不满足才回退候选行尾 / 竖排底部独立行。
        let pager_will_inline = inp.opts.pager_in_preedit
            && !inp.preedit.is_empty()
            && !inp.preedit_embedded
            && inp.pager_visible();
        let s = inp.scale;
        // 字号：base = 生效基准字号（用户 ui.candidate.font_size>0 否则主题 behavior.font_size，
        // 合并见 `refresh_effective_theme`）× DPI；
        // 序号/注释/预编辑按各节点 font_size（相对主字号的有符号逻辑偏移）调整。
        let base_fs = inp.base_font_logical * s;
        let node_fs = |n: &RvNode| (base_fs + n.font_size * s).max(6.0 * s);
        let index_fs = node_fs(&v.index);
        let text_fs = node_fs(&v.text);
        let preedit_fs = node_fs(&v.preedit_bar);
        // 尺寸下限（ui.candidate.min_window_width_* / min_window_height_*，dp×scale）：
        // 施加在**根容器**上，见末尾装配段。此处只取值——`build_tree` 里还有 `min_rows`
        // 那条走内容层（补占位行）的下限，两者并存、互不换算。
        let min_win_w = inp.min_window_w_px() as f32;
        let min_win_h = inp.min_window_h_px() as f32;

        // 颜色：None→兜底。
        let col = |o: Option<[u8; 4]>, d: [u8; 4]| o.unwrap_or(d);
        // 单个 Dim→设备像素（dp×scale）；None→def_logical×scale。
        let dim = |o: Option<Dim>, def_logical: f32| {
            o.map(|x| x.resolve(s, 0.0)).unwrap_or(def_logical * s)
        };
        // RvEdges 四边内边距→设备像素 Edges；逐边 None→对应 def_logical×scale。
        let edges_or = |e: &RvEdges, d: [f32; 4]| Edges {
            t: e.top.map(|x| x.resolve(s, 0.0)).unwrap_or(d[0] * s),
            r: e.right.map(|x| x.resolve(s, 0.0)).unwrap_or(d[1] * s),
            b: e.bottom.map(|x| x.resolve(s, 0.0)).unwrap_or(d[2] * s),
            l: e.left.map(|x| x.resolve(s, 0.0)).unwrap_or(d[3] * s),
        };
        let window_pad = match &self.skin_geom {
            Some(g) => g.insets,
            None => edges_or(&v.window.padding, [6.0, 8.0, 6.0, 8.0]),
        };
        // 内容宽度预算（设备 px）：与渲染入口钳 `content_w` 用的是**同一套算法**（主题
        // vertical_max_width ⊕ 恒生效的屏幕安全网），提前在这里算出来是为了在下面构建候选/
        // 编码栏文字节点时就按预算裁字（带省略号）——而不是等测完整棵树的自然宽度再回头夹
        // 窗口整体尺寸：那样只夹得住窗口 buffer，夹不到文字本身，超宽文字仍会被
        // `draw_text` 的直角缓冲区裁剪一路画到窗口右缘，把圆角盖掉（见
        // project_candidate_screen_safety_width 记忆里的真机实测记录）。
        let content_budget_px: f32 = {
            let (ml, mt, mr, mb) = match self.shadow_params(inp) {
                Some(sh) => sh.margins(),
                None => (0, 0, 0, 0),
            };
            // ⚠️ 这里要的是**局部排版横轴**的上限，旋转态下它是屏幕高度（见函数文档），
            // 于是要扣的阴影扩边也跟着换成上下两边——扣 ml+mr 是拿另一条轴的量在减。
            let shadow_along_axis = if inp.rotated { mt + mb } else { ml + mr };
            let mut bound = inp.local_text_extent_px().saturating_sub(shadow_along_axis);
            if inp.vertical && !inp.candidates.is_empty() {
                let vmax = t.behavior.vertical_max_width;
                if vmax > 0 {
                    bound = bound.min(Self::dp_to_px(vmax as u32, s).max(40));
                }
            }
            // 下限也要同轴：旋转态的局部横轴在屏幕上是高度，取宽度下限等于拿另一条轴的
            // 数当地板——竖屏（工作区高 > 宽）上会把预算抬到超过实际可用长度。
            let floor = if inp.rotated {
                inp.min_window_h_px()
            } else {
                inp.min_window_w_px()
            };
            bound.max(floor) as f32
        };
        let preedit_pad = edges_or(&v.preedit_bar.padding, [3.0, 8.0, 3.0, 8.0]);
        // 文字宽度下限：预算被行内其它成员吃光时每个文字节点仍保底这么宽，避免退化成
        // 「一个字都不显示」。触发它意味着无解（固定开销已超预算），见 [`Self::water_fill`]。
        let min_text_w = 20.0 * s;
        // 状态 patch 取色（selected/hover 的 bg/text，None patch 或缺色→兜底）。
        let patch_bg =
            |p: &Option<Box<RvNode>>, d: [u8; 4]| p.as_ref().and_then(|n| n.bg_color).unwrap_or(d);
        // 状态文字色（与 Go effectiveNode 对齐）：选中优先于悬停；选中/悬停 patch 未给文字色
        // → 回退基态色（不跨态借色）。index/text/comment 同一套消费。
        // 与分段着色的求色共用 `wind_theme::body_color`，两处同一口径（`base` 已是节点基态色）。
        let eff_text = |node: &RvNode, base: [u8; 4], sel: bool, hov: bool| -> [u8; 4] {
            wind_theme::body_color(node, text_state(sel, hov), base)
        };
        // 有效字重（与 eff_text 同构）：节点/item 的状态 patch 字重优先，回退节点/item 基态；0=继承默认。
        // item 参与是因为主题常把"选中加粗"配在 [item.selected].font_weight（如 jidian），需作用到候选文本。
        let eff_weight = |node: &RvNode, item: &RvNode, sel: bool, hov: bool| -> i32 {
            let state = |n: &RvNode| -> Option<i32> {
                let st = if sel {
                    n.selected.as_deref()
                } else if hov {
                    n.hover.as_deref()
                } else {
                    None
                };
                st.map(|s| s.font_weight).filter(|w| *w != 0)
            };
            state(node)
                .or_else(|| state(item))
                .or_else(|| (node.font_weight != 0).then_some(node.font_weight))
                .or_else(|| (item.font_weight != 0).then_some(item.font_weight))
                .unwrap_or(0)
        };
        // 有效边框（base ⊕ selected/hover patch，对齐 Go effectiveNode）：返回设备像素
        // (颜色, 线宽, 圆角)。仅当节点/状态给了 border 色才绘制（如 svgtest text.selected.border）。
        let eff_border = |node: &RvNode, sel: bool, hov: bool| -> Option<([u8; 4], f32, f32)> {
            let st = if sel {
                node.selected.as_deref()
            } else if hov {
                node.hover.as_deref()
            } else {
                None
            };
            let color = st.and_then(|n| n.border_color).or(node.border_color)?;
            let width = st
                .and_then(|n| n.border_width)
                .or(node.border_width)
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or(s)
                .max(1.0);
            let radius = st
                .and_then(|n| n.border_radius)
                .or(node.border_radius)
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or(0.0);
            Some((color, width, radius))
        };
        // 有效背景色：状态 patch ?? 基态。与 eff_border 同构。
        // 独立于 patch_bg（那个只看状态、且带硬兜底），用于「基态也要能配底色」的节点。
        let eff_bg = |node: &RvNode, sel: bool, hov: bool| -> Option<[u8; 4]> {
            let st = if sel {
                node.selected.as_deref()
            } else if hov {
                node.hover.as_deref()
            } else {
                None
            };
            st.and_then(|n| n.bg_color).or(node.bg_color)
        };

        // 通用盒装饰：底色 / 背景图 / 渐变 / z 层覆盖图 / 边框 + 圆角。
        // 每项都是 Option —— 主题未配则原样返回，几何与观感零变化。供此前只有文字色、
        // 连背景边框都没接线的容器节点（candidate_list / footer_bar）复用。
        // 圆角只在显式配了 border_radius 时设置：View 默认 0，不设即方角，与原行为一致。
        let decorate_box = |view: View, node: &RvNode| -> View {
            let mut view = view;
            if let Some(c) = node.bg_color {
                view = view.bg(c);
            }
            if let Some(vi) = inp.rv_image(node.bg_image.as_ref()) {
                view = view.bg_image(vi);
            }
            if let Some(g) = inp.rv_gradient(node.bg_gradient.as_ref()) {
                view = view.bg_gradient(g);
            }
            let layers = inp.rv_layers(&node.layers);
            if !layers.is_empty() {
                view = view.layers(layers);
            }
            if let Some(r) = node.border_radius {
                view = view.radius(r.resolve(s, 0.0));
            }
            if let Some(bc) = node.border_color {
                view = view.border(bc, dim(node.border_width, 0.0).max(1.0));
            }
            view
        };

        // 模式徽标的盒装饰（底色 / 边框 / 圆角 / 内边距）。
        // **横排内嵌与竖排独立 chip 是两条通路**，抽成闭包共用——本仓「同一功能两处装配、
        // 只改一处」是高发 bug 形态。
        // 门控：有底色**或**有边框才渲染成徽标盒（原先只看底色，那样「只要边框的空心徽标」
        // 表达不出来）；两者皆无时保持纯文字，零回归。
        // 圆角一律取 mode_label.border_radius ?? item.border_radius —— **不能用 eff_border
        // 返回的第三个值**，它在未配 border.radius 时兜底 0.0，会把徽标圆角抹平（同 :1824 的坑）。
        let decorate_mode_chip = |chip: View| -> View {
            let border = eff_border(&v.mode_label, false, false);
            if v.mode_label.bg_color.is_none() && border.is_none() {
                return chip;
            }
            let mut chip = chip
                .radius(dim(
                    v.mode_label.border_radius.or(v.item.border_radius),
                    4.0,
                ))
                .pad(edges_or(&v.mode_label.padding, [1.0, 6.0, 1.0, 6.0]));
            if let Some(bg) = v.mode_label.bg_color {
                chip = chip.bg(bg);
            }
            if let Some((bc, bw, _)) = border {
                chip = chip.border(bc, bw);
            }
            chip
        };

        // ── 「与编码同处一行」的两个不截断成员，各自的**节点自身宽度** ──
        //
        // 抽成闭包是因为它们有**三个**消费点，而此前每处各写了一份算术：独立编码栏的文字
        // 预算（正下方）、候选行的 `mode_label_row_w`、翻页栏的 `pager_row_w`。分开写的代价
        // 已经兑现过两次 —— 55032bc0 补了内联编码那条落点却漏了独立栏这条（F-17），
        // 2bee2a9f 改了装配段的右留白却漏了预算段。
        //
        // 只共用**节点这一半**，外边距/间隙留给各调用点：栏行给徽标的是左 12px，候选行给的
        // 是右 `trailing`（且仅在后面真有候选时），而翻页栏在候选行还要额外吃一个 `box_gap`
        // （Row 的 gap），在栏行则没有。方向与条件都不同，硬并进来只会再造一个分叉。
        let mode_chip_node_w = || -> f32 {
            if inp.mode_label.is_empty() {
                return 0.0;
            }
            // 门控与 `decorate_mode_chip` 同源：没底色也没边框时它不包盒子，也就没有内边距。
            let chip_pad = if v.mode_label.bg_color.is_some()
                || eff_border(&v.mode_label, false, false).is_some()
            {
                let p = edges_or(&v.mode_label.padding, [1.0, 6.0, 1.0, 6.0]);
                p.l + p.r
            } else {
                0.0
            };
            inp.text_renderer
                .measure(
                    inp.mode_label,
                    &Self::measure_style(
                        node_fs(&v.mode_label),
                        v.mode_label.font_weight,
                        v.mode_label.font_family.as_deref(),
                    ),
                )
                .width
                + chip_pad
        };
        let pager_node_w = || -> f32 {
            let footer_fs = node_fs(&v.footer_bar);
            let fpad = edges_or(&v.footer_bar.padding, [0.0, 6.0, 0.0, 6.0]);
            let fmargin = edges_or(&v.footer_bar.margin, [0.0, 0.0, 0.0, 8.0]);
            let num_w = if inp.page_number_visible() {
                inp.text_renderer
                    .measure(
                        &format!("{}/{}", inp.page, inp.total_pages),
                        &Self::measure_style(
                            footer_fs,
                            v.footer_bar.font_weight,
                            v.footer_bar.font_family.as_deref(),
                        ),
                    )
                    .width
            } else {
                0.0
            };
            // 两个箭头各占 arrow_slot_w（基线 = 字号 + 左右 padding，多字按文本撑开）——
            // 与下方 pager 装配处共用同一份，不再手写第二遍公式。
            let base = footer_fs + fpad.l + fpad.r;
            let fb = &v.footer_bar;
            inp.arrow_slot_w(arrow_char(&fb.prev_char, "‹"), base, footer_fs, fb)
                + inp.arrow_slot_w(arrow_char(&fb.next_char, "›"), base, footer_fs, fb)
                + num_w
                + fmargin.l
                + fmargin.r
        };

        // **独立**编码栏（`!preedit_embedded`）的文字预算：它是 root 这个 Column 的直接子节点、
        // 自成一行，不与候选抢宽度，故只需扣窗口内边距与自身内边距——**以及同一行右端那两个
        // 不截断的成员**。
        // 内联编码（`preedit_embedded`）走的是另一条路——它在候选行里，预算由下方的统一分配
        // （横排 water-filling）给出。
        //
        // ★ 右端预留（F-17）：band 排的是 `[编码][spacer][徽标][翻页栏]`，后两者是纯加在编码
        // 后面的，此前一个都没进预算。超长编码下 `root` 就按超宽排完了版，而窗口宽那道钳制
        // （见渲染入口 `content_w.min(safety_max_w)`）是**裁切**语义、不回头改 `root`，于是
        // 溢出的部分在右缘直接被切掉且没有省略号 —— 翻页栏和徽标看不见，或只剩半截箭头。
        // 实测（scale=1，200 组 `a'`，上限 3000）：光翻页栏就撑到 3091.6，加上徽标 3146.8。
        //
        // ⚠️ 旋转态下 `content_budget_px` 量的是**局部横轴**（屏幕高度），而独立编码栏在旋转
        // 包裹**之外**、量的是屏幕宽度——这里是两条轴混用。竖屏上长 preedit 会拿到超过屏幕
        // 宽度的预算而不被截断（窗口本身仍被渲染期的宽度钳制夹住，夹不到文字）。
        // 已知边界，蒙文用户是横屏，暂不为它再分一条轴。
        let preedit_bar_right_reserve_px = {
            // 徽标的 12px 是它在 band 里的**左**外边距（把它与编码拉开），与候选行那个右留白
            // 不是同一笔账——那边由 `mode_label_row_w` 按「后面有没有候选」决定。
            let chip = if inp.mode_label.is_empty() {
                0.0
            } else {
                12.0 * s + mode_chip_node_w()
            };
            // band 里翻页栏直接挂在 spacer 右侧，没有容器 gap 可吃（候选行那条才有 box_gap）。
            let pager = if pager_will_inline {
                pager_node_w()
            } else {
                0.0
            };
            chip + pager
        };
        let preedit_bar_text_budget_px = (content_budget_px
            - window_pad.l
            - window_pad.r
            - preedit_pad.l
            - preedit_pad.r
            - preedit_bar_right_reserve_px)
            .max(min_text_w);

        // 皮肤生效：根容器的盒即窗体——不画底色 / 边框 / 圆角（外形由皮肤图决定），
        // 内边距换成皮肤内容框，最小尺寸取窗体固定段之和。
        let skin = self.skin_geom.as_ref();
        let (skin_min_w, skin_min_h) = skin.map_or((0.0, 0.0), |g| (g.min_w, g.min_h));
        let mut root = match skin {
            Some(_) => View::container(Layout::Column).pad(window_pad),
            None => View::container(Layout::Column)
                .bg(col(v.window.bg_color, wind_theme::fallback::WINDOW_BG))
                .border(
                    col(v.window.border_color, [200, 200, 200, 200]),
                    dim(v.window.border_width, 1.0).max(1.0),
                )
                .radius(dim(v.window.border_radius, 8.0))
                .clip_to_corners()
                .pad(window_pad),
        }
        // band 间距（预编辑条↔候选列表↔翻页）= 主题 candidate_list.band_gap，未配→0（与 Go 一致）。
        .gap(dim(v.window_gap, 0.0))
        // ── 窗口尺寸下限（ui.candidate.min_window_width_* / min_window_height_*）──
        // 下限量的是整个窗口，故落在根容器上：候选、编码栏、翻页栏都在里面，凑不满的
        // 部分留空，候选自身照常按内容排布（不拉伸、不改间距）。
        //
        // ⛔ 不要改成在渲染入口钳 `content_w`/`content_h`：那是 `root.measured_size()`
        // 之后的产物，调大它不会回头改 root 的 mw/mh ⇒ 窗口缓冲变大而 root 背景/边框仍按
        // 旧尺寸画，边上露一条透明带，竖排 `fill_cross` 的高亮也不跟着撑。上限（主题
        // `vertical_max_width`）能那么写是因为它是裁切语义，下限不是。
        .min_w(min_win_w.max(skin_min_w))
        .min_h(min_win_h.max(skin_min_h))
        // 高度富余落在**顶部**（仅上翻时）：窗口在光标上方时底边贴光标，空白压在下面
        // 会把候选整体顶离光标，位置反而随内容抖动——正是本功能要消除的东西。
        // 与 min_rows 的占位行「反转时补顶部」同一判据。
        .main(if above { Align::End } else { Align::Start });
        // 窗口背景图 / 渐变 / z 层：皮肤生效时由皮肤接管，不再叠。
        if skin.is_none() {
            // 窗口背景图（九宫格/拉伸位图皮肤，如 jidian 的 panel）。
            if let Some(vi) = inp.rv_image(v.window.bg_image.as_ref()) {
                root = root.bg_image(vi);
            }
            // 窗口背景渐变（叠在底色上、背景图下）。
            if let Some(g) = inp.rv_gradient(v.window.bg_gradient.as_ref()) {
                root = root.bg_gradient(g);
            }
            // 窗口 z 层覆盖图（如 jidian 右下角 mark 水印）。
            let win_layers = inp.rv_layers(&v.window.layers);
            if !win_layers.is_empty() {
                root = root.layers(win_layers);
            }
        }

        // 预编辑行（主题背景带 + 文本色）。完整渲染 preedit_bar 自身背景：底色 + 位图背景 +
        // z 层 + 边框（此前只画底色，位图主题的 preedit 背景/边框丢失）。
        // 嵌入模式（preedit_embedded）下不画独立条——编码作为候选行首单元内联（见下方 list 构建）。
        // 【延迟装配】编码栏与分隔线不在此直接 push root，先存入变量；末尾装配段据 swap_bands 决定
        // 放窗口顶部（正常）或底部（编码沉底贴光标），并据 pager_will_inline 追加翻页栏到栏行右端。
        let mut preedit_band: Option<View> = None;
        let mut preedit_sep: Option<View> = None;
        if !inp.preedit.is_empty() && !inp.preedit_embedded {
            let mut band = View::container(Layout::Row)
                .cross(Align::Center)
                .bg(col(v.preedit_bar.bg_color, [240, 240, 240, 255]))
                // 圆角：preedit_bar 自己的优先，未配才跟随 item（历史行为——此前只读
                // item.border_radius，调候选项圆角会连带改预编辑栏）。`.or()` 保回退链，
                // 老主题零变化。
                .radius(dim(
                    v.preedit_bar.border_radius.or(v.item.border_radius),
                    4.0,
                ))
                .pad(preedit_pad)
                .margin(edges_or(&v.preedit_bar.margin, [0.0; 4]))
                // 独立编码栏在旋转包裹层**外面**，永远不扶正。
                .child(inp.preedit_view(preedit_fs, preedit_bar_text_budget_px, false));
            if let Some(vi) = inp.rv_image(v.preedit_bar.bg_image.as_ref()) {
                band = band.bg_image(vi);
            }
            if let Some(g) = inp.rv_gradient(v.preedit_bar.bg_gradient.as_ref()) {
                band = band.bg_gradient(g);
            }
            let band_layers = inp.rv_layers(&v.preedit_bar.layers);
            if !band_layers.is_empty() {
                band = band.layers(band_layers);
            }
            if let Some(bc) = v.preedit_bar.border_color {
                band = band.border(bc, dim(v.preedit_bar.border_width, 0.0).max(1.0));
            }
            // 右对齐区（mode_label / 并入的翻页栏）：band 跨轴撑满窗口内容宽 + spacer 吸收中间空白，
            // 才能把右侧内容顶到栏行末尾。二者任一存在即启用（排布：[编码] spacer [mode_label] [翻页栏]）。
            let need_right_align = !inp.mode_label.is_empty() || pager_will_inline;
            // 编码栏恒撑满窗口内容宽（与主题编辑器预览 `stretch: true` 一致）。曾经只在
            // 清风设计主题 / 右对齐区存在时才撑满，第三方主题的编码栏就只有编码那么宽，
            // 配了背景色后后半截露出窗口底（窗口透明时就是透明的一段）。
            band = band.fill_cross();
            if need_right_align {
                band = band.child(View::spacer());
            }
            // 模式标记：右对齐到栏行末尾（在翻页栏之左）；字号取主题 mode_label 配置。
            if !inp.mode_label.is_empty() {
                let ml_fs = node_fs(&v.mode_label);
                let chip = decorate_mode_chip(
                    View::leaf(
                        inp.mode_label,
                        col(v.mode_label.text_color, [120, 120, 128, 255]),
                    )
                    .font_size(ml_fs)
                    .font_weight(v.mode_label.font_weight)
                    .font_family(v.mode_label.font_family.clone())
                    .margin(Edges {
                        l: 12.0 * s,
                        ..Edges::default()
                    }),
                );
                band = band.child(chip);
            }
            // 翻页栏并入（pager_will_inline）：在末尾装配段追加到此 band 末尾（spacer 右侧）。
            // 编码栏与候选区之间的分隔线（`views.divider`）：颜色缺省取调色板 `separator`
            // （清风设计主题定义），都没有就不画；`enabled = false` 强制不画。
            // 无 views 块的主题拿到的是全默认 RvViews（bg_color 空），退回直接按名取调色板。
            let sep_col = v
                .divider
                .bg_color
                .unwrap_or_else(|| t.color("separator", [0, 0, 0, 0]));
            let preedit_designed = sep_col[3] > 0 && v.divider_enabled != Some(false);
            preedit_band = Some(band);
            if preedit_designed {
                // 线粗取整：这是「就该 N 个逻辑像素粗」的语义定宽元素，非整数尺寸
                // 交给 `snap_to_pixels` 会随落点在 1px/2px 之间变（同 caret_w）。
                let thick = v
                    .divider_height
                    .map(|d| d.resolve(s, 0.0))
                    .unwrap_or(s)
                    .round()
                    .max(1.0);
                let mut line = View::container(Layout::Row)
                    .bg(sep_col)
                    .fixed_h(thick)
                    .fill_cross()
                    .margin(edges_or(&v.divider.margin, [0.0, 0.0, 4.0, 0.0]));
                if let Some(r) = v.divider.border_radius {
                    line = line.radius(r.resolve(s, 0.0));
                }
                preedit_sep = Some(line);
            }
        }

        // 候选项颜色（基态）。状态色（选中/悬停）逐项经 eff_text 计算。
        let text_color = col(v.text.text_color, TEXT_FALLBACK);
        let sel_bg = patch_bg(&v.item.selected, wind_theme::fallback::SELECTED_BG);
        let hover_bg = patch_bg(&v.item.hover, wind_theme::fallback::HOVER_BG);
        let index_color = col(v.index.text_color, [66, 133, 244, 255]);
        let comment_color = col(v.comment.text_color, COMMENT_FALLBACK);
        let comment_fs = node_fs(&v.comment);
        let index_circle = v.index.bg_shape == "circle";
        let index_circle_bg = col(v.index.bg_color, [66, 133, 244, 255]);
        let item_pad = edges_or(&v.item.padding, [7.0, 10.0, 7.0, 8.0]);
        let item_radius = dim(v.item.border_radius, 4.0);
        // 候选间距全由主题决定（与 Go 对齐，以主题配置为准）：
        // 横排候选框间隙 = max(item_spacing − item 左右内边距, 0)——内边距本身已提供视觉间隔，
        // 扣除避免"间距 + 内边距"叠加（旧逻辑用 config×2 凭空多加一段，致 msime 元素间隔不一致）。
        // 竖排行距 = candidate_list.row_gap。主题未配 → 0（候选框相邻，靠 padding 分隔）。
        let item_spacing = v.item_spacing.map(|d| d.resolve(s, 0.0)).unwrap_or(0.0);
        let box_gap = (item_spacing - item_pad.l - item_pad.r).max(0.0);
        let row_gap_v = dim(v.row_gap, 0.0);
        // 选中候选左侧强调条（仅主题启用时，如 msime/jidian）。
        // height_ratio 判零回退：RvViews 若未经 resolve 填充（Default 构造）该字段是 0.0，
        // 直接乘会把条高算成 0 → 钳到 2px 细线，观感是强调条消失。
        let accent_bar = v.accent_bar_enabled.then(|| LeftBar {
            color: col(v.accent_bar.bg_color, [66, 133, 244, 255]),
            width: dim(v.accent_bar_width, 3.0),
            height_ratio: if v.accent_bar_height_ratio > 0.0 {
                v.accent_bar_height_ratio
            } else {
                DEFAULT_ACCENT_BAR_HEIGHT_RATIO
            },
            offset: dim(v.accent_bar_offset, 0.0),
        });

        // 候选列表：横排=Row（cell 并列）；竖排=Column（候选纵向堆叠）。
        // 无候选时（仅提示徽标/preedit，无候选可纵向堆叠）强制用 Row：徽标 + 等高占位
        // 并排成单行，高度 = 一个候选行，与横排一致；否则竖排下徽标行与占位行会纵向
        // 堆叠致提示窗口过高（网址/临拼/临英刚进入时尤甚）。preedit/徽标分隔方向亦随之。
        // ★ 旋转态也走竖排这一支：它在**局部未旋转空间**里就是候选纵向堆叠，转 90° 后
        // 才成为并列的列。于是行距、宽度预算、文字截断、分隔方向全部沿用竖排的既有实现，
        // 一行都不用改——这正是把旋转做成「两位」而不是「第四种排列」的收益所在。
        let list_vertical = (inp.vertical || inp.rotated) && !inp.candidates.is_empty();
        // 展开态（多行网格）：只在横排出现（协调器那边已按方向把关，这里再判一次是因为
        // 方向与网格是两条命令下发的，中间可能隔着一帧）。`cols == 0` 是无意义的版式，按收起处理。
        let grid = inp
            .grid
            .filter(|g| g.cols > 0 && !list_vertical && !inp.rotated && !inp.candidates.is_empty());
        let grid_cols = grid.map_or(1, |g| g.cols as usize);
        let list_pad = edges_or(&v.candidate_list.padding, [0.0; 4]);
        // candidate_list 此前只贡献 gap/row_gap/band_gap 三个间距，背景与边框从不接线，
        // 「候选区与编码栏用不同底色分区」这类设计表达不出来。装饰全 Option，未配零回归。
        let mut list = decorate_box(
            if list_vertical {
                // fill_cross 让候选列撑满 root 宽度（= max(最宽候选, 翻页栏)），
                // 再配合各候选 item 的 fill_cross，所有候选高亮宽度统一。
                View::container(Layout::Column).gap(row_gap_v).fill_cross()
            } else if grid.is_some() {
                // 网格：一列行，每行是一页候选（行在装配段由 `grid_cells` 切出来）。
                View::container(Layout::Column).gap(row_gap_v)
            } else {
                View::container(Layout::Row)
                    .gap(box_gap)
                    .cross(Align::Center)
            }
            // 候选区内边距（默认全 0=零回归）。footer_bar 不加这项——它的 padding
            // 已被翻页箭头消费（fpad），容器再加一层会双重生效。
            .pad(list_pad),
            &v.candidate_list,
        );

        // ══ 文字宽度预算分配 ══
        // 竖排：每候选独占一行、互不竞争，各自可用满整行预算。
        // 横排：内联编码 + 候选们 + 模式徽标 + 翻页栏全挤在同一个 Row 里共享一行宽度，
        //   必须按最大最小公平（water-filling）分配，理由与「为什么不能贪心」见
        //   [`Self::water_fill`]。徽标与翻页栏不截断（本就短、且截了没意义），按实际宽度
        //   从预算里**预留**掉。
        let text_pad = edges_or(&v.text.padding, [0.0; 4]);
        // 逆序仅改排列顺序；i 仍是原始索引（tag/标签/选中据此）。提前到此是因为下面的预扫
        // 要按最终顺序算每个候选的固定开销。
        //
        // ★ 旋转态**恒逆序**：局部列首经顺时针 90° 会落到屏幕最右，而蒙古文的列是从左向右
        // 推进的（候选 1 要在最左）。逆序后局部列尾 = 屏幕最左，正是候选 1。
        // 与 `flip_cands` 合成一个布尔，是为了让下面「占位行补在哪一端」自动跟着走——
        // 两处各判一次的话，旋转态的占位列会补在候选 1 那一侧，把候选 1 推离窗口边缘。
        let reversed = flip_cands || inp.rotated;
        let mut order: Vec<(usize, &CandidateItem)> = inp.candidates.iter().enumerate().collect();
        if reversed {
            order.reverse();
        }
        // 每候选正文的显示副本（与 `order` 同序）：模糊音提示「苹(ping)果」按含提示的实宽参与
        // 预算分配（网格列宽同理），截断时提示整段保留或整段去掉（`truncate_candidate_styled`）。
        let displays: Vec<StyledText> = order.iter().map(|(_, c)| cand_display(c)).collect();
        // 每候选的「固定开销」（除文字外无条件占用的宽度）与「自然文字宽」。
        // 一律用 [`Self::measure_style`] 按各节点真实字族/字重量，与渲染同源。
        let gap_w = if list_vertical { 0.0 } else { box_gap };
        // 同时记下每候选注释的 (总宽, 非文字开销=注释内外边距)，竖排「主词条优先」分配要用。
        type WidthPairs = Vec<(f32, f32)>;
        let (cand_metrics, cand_comment_w): (WidthPairs, WidthPairs) = order
            .iter()
            .zip(&displays)
            .map(|((i, cand), display)| {
                let is_sel = *i == inp.selected;
                let is_hover = inp.hover >= 0 && inp.hover as usize == *i;
                let idx_w = if cand.no_index {
                    0.0
                } else {
                    let marker = if cand.label.is_empty() {
                        (i + 1).to_string()
                    } else {
                        cand.label.clone()
                    };
                    let ip = edges_or(&v.index.padding, [0.0; 4]);
                    let im = edges_or(&v.index.margin, [0.0; 4]);
                    let base = if index_circle {
                        // 圆圈样式是 fixed_w 方块，与文字宽度无关。
                        (index_fs * 1.5).round()
                    } else {
                        inp.text_renderer
                            .measure(
                                &marker,
                                &Self::measure_style(
                                    index_fs,
                                    eff_weight(&v.index, &v.item, is_sel, is_hover),
                                    v.index.font_family.as_deref(),
                                ),
                            )
                            .width
                    };
                    base + ip.l + ip.r + im.l + im.r
                };
                let comment = right_comment(cand, inp.rotated);
                let (comment_w, comment_box) = if comment.is_empty() {
                    (0.0, 0.0)
                } else {
                    let cp = edges_or(&v.comment.padding, [0.0; 4]);
                    let cm = edges_or(&v.comment.margin, [0.0, 0.0, 0.0, 6.0]);
                    let bx = cp.l + cp.r + cm.l + cm.r;
                    let tw = inp
                        .text_renderer
                        .measure(
                            comment.as_str(),
                            &Self::measure_style(
                                comment_fs,
                                eff_weight(&v.comment, &v.item, is_sel, is_hover),
                                v.comment.font_family.as_deref(),
                            ),
                        )
                        .width;
                    (tw + bx, bx)
                };
                let tm = if cand.no_index {
                    Edges::default()
                } else {
                    edges_or(&v.text.margin, [0.0, 0.0, 0.0, 4.0])
                };
                let fixed = item_pad.l
                    + item_pad.r
                    + idx_w
                    + comment_w
                    + text_pad.l
                    + text_pad.r
                    + tm.l
                    + tm.r
                    + gap_w;
                let natural = inp
                    .text_renderer
                    .measure(
                        display.as_str(),
                        &Self::measure_style(
                            text_fs,
                            eff_weight(&v.text, &v.item, is_sel, is_hover),
                            text_family.as_deref(),
                        ),
                    )
                    .width;
                ((fixed, natural), (comment_w, comment_box))
            })
            .unzip();
        // 整行可用宽度（已扣窗口与候选区内边距）。
        let row_budget =
            (content_budget_px - window_pad.l - window_pad.r - list_pad.l - list_pad.r).max(0.0);
        // 横排下内联编码也在同一行里跟候选抢宽度，故作为分配参与者一并纳入；竖排它自成一行，
        // 走独立预算不参与竞争。
        let preedit_competes =
            !list_vertical && grid.is_none() && inp.preedit_embedded && !inp.preedit.is_empty();
        let preedit_natural = if inp.preedit.is_empty() {
            0.0
        } else {
            inp.text_renderer
                .measure(
                    inp.preedit,
                    &Self::measure_style(
                        preedit_fs,
                        v.preedit_bar.font_weight,
                        v.preedit_bar.font_family.as_deref(),
                    ),
                )
                .width
        };
        // 横排行内两个**不截断**的成员，按实际宽度预留：
        //  - 模式徽标：短（拼/双/英…），截了反而认不出
        //  - 翻页栏：定宽（两个箭头 + 页码），截了就没法点
        let preedit_bar_shown_pre = !inp.preedit.is_empty() && !inp.preedit_embedded;
        let mode_label_row_w =
            if !list_vertical && !inp.mode_label.is_empty() && !preedit_bar_shown_pre {
                mode_chip_node_w()
                // 与装配段的 `trailing` **同源同条件**：无候选时那边给 0，这边再无条件加
                // 12 就会把可用宽算少，超长编码提前一截。两处分开写过一次，正是 GH#127
                // 只修好一半的原因。
                + if inp.candidates.is_empty() { 0.0 } else { 12.0 * s }
                // ⚠️ `box_gap` 这一项照加：无候选时 `list` 里仍是 [徽标, 占位行] 两个子节点，
                // Row 的 gap 照样落在它们之间。内置主题一律未配 `item_spacing`（box_gap=0），
                // 故观感上看不出来；自定义主题把它配到大于 item 左右内边距之和时，提示窗
                // 右边会按差值多出一小块 —— 是 GH#127 的同类残余，但去掉 gap 会连带动到
                // 内联编码与徽标之间的间隔，故留着不动。
                + box_gap
            } else {
                0.0
            };
        // 翻页栏并入**内联**编码那一行（右对齐）——与 `pager_will_inline` 同一个开关、同一个
        // 诉求（竖排省一行），只是编码不在独立栏里，落点改为编码自己那一行的右端。
        // · 仅竖排：横排下内联编码与候选同属一行，翻页栏本就落在该行末尾，开关无从谈起。
        // · ⚠️ 排除旋转态（蒙文）：内联编码是 `list` 的子节点、在旋转包裹层**内部**，翻页栏跟
        //   进去会连箭头和页码一起转 90°。那一档维持底部独立行（它加在 `root` 上，屏幕上仍是横的）。
        let pager_in_inline_row = inp.opts.pager_in_preedit
            && !inp.preedit.is_empty()
            && inp.preedit_embedded
            && list_vertical
            && !inp.rotated
            && inp.pager_visible();
        // 同一行里要为翻页栏预留的宽度（0 = 这一行没有翻页栏）：
        //   · 横排未并入编码栏 → 翻页栏落在候选行尾，与候选抢宽度（原行为）
        //   · 竖排内联编码并入 → 翻页栏在编码那一行右端，与编码抢宽度
        let pager_row_w = if inp.pager_visible()
            && ((!list_vertical && grid.is_none() && !pager_will_inline) || pager_in_inline_row)
        {
            // `box_gap` 是**候选行这个 Row 的 gap**，只有这条落点吃得到；独立编码栏那条
            // 挂在 spacer 右侧、没有容器间隙，故它只取节点自身宽（见 `pager_node_w` 上方）。
            pager_node_w() + box_gap
        } else {
            0.0
        };
        // 横排内联编码在候选行里的固定开销（下面的分配与上方注释条的余量计算共用）。
        let preedit_fixed = if preedit_competes {
            preedit_pad.l + 16.0 * s + box_gap // 左缩进 + 装配段右留白 + 间隙
        } else {
            0.0
        };
        // ══ 网格列宽（展开态）══
        // 列宽 = 可见区里这一列各格自然宽的最大值，各列对齐成表格；表格总宽超出行预算时，
        // 各列宽按 water-filling 公平缩减（与单行按候选分配是同一个算法，只是参与者从
        // 「每个候选」换成「每一列」）。格子再按所在列的宽度截文字、并以它为宽度下限。
        // 格宽要扣掉 item 外边距与格间距：它们在格子外面，不归任何一列。
        let grid_col_w: Vec<f32> = if grid.is_some() {
            let item_margin = edges_or(&v.item.margin, [0.0; 4]);
            let mut demand = vec![0.0f32; grid_cols];
            // 各列的固定开销上限（序号、内边距、注释——都不截）：列宽再缩也不能小于它加
            // 一截文字保底，否则格子按内容撑开、比列宽宽，这一行就和别的行错开了。
            let mut floor = vec![0.0f32; grid_cols];
            for (k, (fixed, natural)) in cand_metrics.iter().enumerate() {
                let c = k % grid_cols;
                demand[c] = demand[c].max(fixed - gap_w + natural);
                floor[c] = floor[c].max(fixed - gap_w + min_text_w);
            }
            let avail = row_budget
                - box_gap * (grid_cols - 1) as f32
                - (item_margin.l + item_margin.r) * grid_cols as f32;
            Self::water_fill(&demand, avail, min_text_w)
                .into_iter()
                .zip(floor)
                .map(|(w, f)| w.max(f))
                .collect()
        } else {
            Vec::new()
        };
        // 分配：参与者 = [内联编码(仅横排下与候选同行)] + 候选们。
        // 竖排的注释文字预算（与 `order` 同序；`None` = 不截，横排与直立态恒为 `None`）。
        let mut cand_comment_budgets: Vec<Option<f32>> = vec![None; cand_metrics.len()];
        let (inline_preedit_budget_px, cand_text_budgets) = if list_vertical {
            // 竖排：内联编码与每个候选各占一行、互不竞争，都用满整行预算（保持既有行为）。
            // 唯一例外是翻页栏并进编码行时（`pager_in_inline_row`，此时 `pager_row_w` 非 0）：
            // 那一行被它占掉一截，编码预算须同步扣除，否则超长编码会把翻页栏顶出窗口右缘。
            //
            // ★ 行内「主词条优先、注释让步」（论坛 t271）。行宽上限 R = `row_budget`，来自
            //   `content_budget_px` = min(屏幕工作区宽, 主题 `behavior.vertical_max_width`)
            //   ⊕ 竖排最小宽度兜底，再扣窗口/候选区内边距。记
            //     F = 除注释外的固定开销（item 内边距 + 序号 + 文字内外边距），
            //     T = 文字自然宽，C = 注释总宽（含注释内外边距 B），
            //     Cmin = min(C, B + min_text_w) —— 注释的保底（至少露出一截加 `…`）。
            //   文字预算 = max(R − F − Cmin, min_text_w)：文字先拿，只给注释留保底；
            //   注释文字预算 = max(R − F − min(T, 文字预算) − B, min_text_w)：用文字排完剩下的。
            //   两者都放得下时（T + C ≤ R − F）文字与注释都不截，与改动前逐字节一致。
            //   此前是「文字预算 = R − F − C」，注释从不让步，超长注释把主词条挤成「你…」。
            // ⚠️ 直立态（逐格扶正）不走本规则、保持旧行为：那里注释沿堆叠轴量的是高度，而
            //   `cand_comment_w` 是按横向宽度量的，两套尺子混算会误截。
            let text_budgets = cand_metrics
                .iter()
                .zip(&cand_comment_w)
                .zip(cand_comment_budgets.iter_mut())
                .map(|(((fixed, natural), &(c_w, c_box)), c_budget)| {
                    if inp.upright || c_w <= 0.0 {
                        return (row_budget - fixed).max(min_text_w);
                    }
                    let base = fixed - c_w;
                    let reserve = c_w.min(c_box + min_text_w);
                    let text_budget = (row_budget - base - reserve).max(min_text_w);
                    *c_budget = Some(
                        (row_budget - base - natural.min(text_budget) - c_box).max(min_text_w),
                    );
                    text_budget
                })
                .collect::<Vec<f32>>();
            (
                (row_budget - preedit_pad.l - pager_row_w).max(min_text_w),
                text_budgets,
            )
        } else if grid.is_some() {
            // 网格：每格的文字拿「所在列宽 − 本格固定开销」。内联编码独占一行，用满整行。
            let budgets = cand_metrics
                .iter()
                .enumerate()
                .map(|(k, (fixed, _))| {
                    (grid_col_w[k % grid_cols] - (fixed - gap_w)).max(min_text_w)
                })
                .collect();
            ((row_budget - preedit_pad.l).max(min_text_w), budgets)
        } else {
            let fixed_sum: f32 = cand_metrics.iter().map(|(f, _)| *f).sum();
            let avail = row_budget - mode_label_row_w - pager_row_w - fixed_sum - preedit_fixed;
            let mut demands: Vec<f32> = Vec::with_capacity(cand_metrics.len() + 1);
            if preedit_competes {
                demands.push(preedit_natural);
            }
            demands.extend(cand_metrics.iter().map(|(_, n)| *n));
            let alloc = Self::water_fill(&demands, avail, min_text_w);
            if preedit_competes {
                (alloc[0], alloc[1..].to_vec())
            } else {
                ((row_budget - preedit_pad.l).max(min_text_w), alloc)
            }
        };

        // ══ 上方注释条（设计 candidate-comment-above-line.md §3.3）══
        // 页级判定：本页任一候选带上段，整页 item 都改成 Column[上方条, 主行]，没有上段的候选
        // 放透明占位叶子——整页等高，竖排不出现行高锯齿。旋转 / 直立态不支持，上段已由
        // `right_comment` 合回右侧注释。`has_above` 为假时以下各处一行都不走，树与改动前逐字节一致。
        let has_above = !inp.rotated && inp.candidates.iter().any(|c| !c.comment_above.is_empty());
        let above_fs = node_fs(&v.comment_above);
        let above_family = v.comment_above.font_family.clone();
        let above_pad = edges_or(&v.comment_above.padding, [0.0; 4]);
        let above_margin = edges_or(&v.comment_above.margin, [0.0; 4]);
        // 每候选（与 `order` 同序）：(左缩进, 上段文字宽度上限)。
        // · 左缩进 = item 内容区起点到候选文字字形起点：序号外框 + 文字左外边距 + 文字左内边距。
        // · 上段只做 UI 兜底截断，**不参与**候选文字的 water_fill（文字预算与无上方条时相同）：
        //   竖排每行独占 row_budget；横排只能用主行排完后整行剩下的余量（多条超长上段之间再按
        //   water_fill 公平分），故上段再长也不会把窗口顶出 row_budget。
        let above_cols: Vec<(f32, f32)> = if has_above {
            let per: Vec<(f32, f32, f32, f32)> = order
                .iter()
                .zip(&cand_metrics)
                .zip(&cand_text_budgets)
                .map(|(((i, cand), (fixed, natural)), budget)| {
                    let is_sel = *i == inp.selected;
                    let is_hover = inp.hover >= 0 && inp.hover as usize == *i;
                    let (idx_box, tm_l) = if cand.no_index {
                        (0.0, 0.0)
                    } else {
                        let ip = edges_or(&v.index.padding, [0.0; 4]);
                        let im = edges_or(&v.index.margin, [0.0; 4]);
                        let marker = if cand.label.is_empty() {
                            (i + 1).to_string()
                        } else {
                            cand.label.clone()
                        };
                        // 与装配段同构：圆圈序号 `fixed_w` 覆盖含内边距的整宽。
                        let core = if index_circle {
                            (index_fs * 1.5).round()
                        } else {
                            inp.text_renderer
                                .measure(
                                    &marker,
                                    &Self::measure_style(
                                        index_fs,
                                        eff_weight(&v.index, &v.item, is_sel, is_hover),
                                        v.index.font_family.as_deref(),
                                    ),
                                )
                                .width
                                + ip.l
                                + ip.r
                        };
                        (
                            core + im.l + im.r,
                            edges_or(&v.text.margin, [0.0, 0.0, 0.0, 4.0]).l,
                        )
                    };
                    let indent = idx_box + tm_l + text_pad.l;
                    // 上方条除文字外的横向开销（item 左右内边距 + 缩进 + 上方条自身边距）。
                    let over = item_pad.l
                        + item_pad.r
                        + indent
                        + above_pad.l
                        + above_pad.r
                        + above_margin.l
                        + above_margin.r;
                    let above_w = if cand.comment_above.is_empty() {
                        0.0
                    } else {
                        inp.text_renderer
                            .measure(
                                cand.comment_above.as_str(),
                                &Self::measure_style(
                                    above_fs,
                                    eff_weight(&v.comment_above, &v.item, is_sel, is_hover),
                                    above_family.as_deref(),
                                ),
                            )
                            .width
                    };
                    // 主行实宽（不含横排间隙）：文字按分到的预算截、放得下就是自然宽。
                    let main_w = fixed - gap_w + natural.min(*budget);
                    (indent, over, above_w, main_w)
                })
                .collect();
            if list_vertical {
                per.iter()
                    .map(|&(indent, over, _, _)| (indent, row_budget - over))
                    .collect()
            } else if grid.is_some() {
                // 网格：上段与主行同在一格里，宽度上限就是所在列宽。
                per.iter()
                    .enumerate()
                    .map(|(k, &(indent, over, _, _))| (indent, grid_col_w[k % grid_cols] - over))
                    .collect()
            } else {
                let used: f32 = cand_metrics
                    .iter()
                    .zip(&cand_text_budgets)
                    .map(|((fixed, natural), budget)| fixed + natural.min(*budget))
                    .sum::<f32>()
                    + mode_label_row_w
                    + pager_row_w
                    + preedit_fixed
                    + if preedit_competes {
                        preedit_natural.min(inline_preedit_budget_px)
                    } else {
                        0.0
                    };
                let extra_demands: Vec<f32> = per
                    .iter()
                    .map(|&(_, over, above_w, main_w)| (over + above_w - main_w).max(0.0))
                    .collect();
                let extra = Self::water_fill(&extra_demands, (row_budget - used).max(0.0), 0.0);
                per.iter()
                    .zip(extra)
                    .map(|(&(indent, over, _, main_w), e)| (indent, main_w + e - over))
                    .collect()
            }
        } else {
            Vec::new()
        };
        // 没有上段的候选 / 占位行用的透明占位叶子：字号、字重、字族、边距与真实上方条同构，
        // 只为撑出同样的行高（真实后端行高取自字族 line metrics，缺一项就矮一截）。
        let above_ghost = |sel: bool, hov: bool| {
            View::leaf(" ".to_string(), [0, 0, 0, 0])
                .font_size(above_fs)
                .font_weight(eff_weight(&v.comment_above, &v.item, sel, hov))
                .font_family(above_family.clone())
                .pad(above_pad)
                .margin(above_margin)
        };
        // 横排里一行高的非 item 成员（翻页栏 / 模式徽标 / 内联编码）要补的上外边距 = 上方条外框高。
        // 横排 list 是 Row.cross(Center)：它们原本相对两行高的 item 居中，会比主行高出半个上方条；
        // 补上这一截后，其外框与 item 等高位移、内容恰好落在主行中线上。竖排它们各占一行，不受影响。
        // 度量与 `above_ghost` 同源（字号 / 字重 / 字族 + 上下内外边距；边框不占布局尺寸）。
        let above_lift = if has_above && !list_vertical && grid.is_none() {
            inp.text_renderer
                .measure(
                    " ",
                    &Self::measure_style(
                        above_fs,
                        eff_weight(&v.comment_above, &v.item, false, false),
                        above_family.as_deref(),
                    ),
                )
                .height
                + above_pad.t
                + above_pad.b
                + above_margin.t
                + above_margin.b
        } else {
            0.0
        };
        // 把已装好 [序号, 文字, 注释] 的 Row 型 item 改成 Column[上方条, 主行]：
        // item 自身的装饰（内外边距 / 圆角 / tag / 背景 / 边框 / 选中条）原样留在外层，包住两行。
        let stack_above = |mut item: View, above: View| {
            let mut main = View::container(Layout::Row).cross(Align::Center);
            main.children = std::mem::take(&mut item.children);
            item.layout = Layout::Column;
            item.cross_align = Align::Start;
            item.child(above).child(main)
        };

        // 【构建位置】翻页器在此提前构建（原先在候选项装配之后）：竖排内联编码要把它作为
        // **子节点**并入编码那一行，而那一行在候选项之前就装配完毕 —— 消费点在前，构建必须更前。
        // 其余落点（编码栏 band / 候选行尾 / 竖排底部独立行）都在末尾装配段用 take() 转移，
        // 与构建位置无关。
        // 翻页器（多页时）：‹ p/t › —— 箭头可点击翻页，带悬停高亮 + 禁用态
        // mut：末尾装配段据归属（并入编码栏 / 候选行尾 / 竖排底部）用 take() 转移所有权。
        let mut pager = if inp.pager_visible() {
            // 禁用态箭头色：优先主题给 footer_bar 配的禁用态文字色（编辑器「禁用态」面板
            // 就是配它），未配才回退全局 text_hint —— 回退保持旧观感，内置主题都没配。
            let disabled = v
                .footer_bar
                .disabled
                .as_ref()
                .and_then(|d| d.text_color)
                .unwrap_or_else(|| t.color("text_hint", [180, 180, 185, 255]));
            let marker_c = t.color("text_dim", [140, 140, 145, 255]);
            let accent = col(v.accent_bar.bg_color, [66, 133, 244, 255]);
            // 文字箭头启用色：优先 footer_bar.text_color，回退 accent。
            let arrow_on = col(v.footer_bar.text_color, accent);
            let footer_fs = node_fs(&v.footer_bar);
            let prev_on = inp.page > 1;
            let next_on = inp.page < inp.total_pages;
            // 固定矩形触摸区（对齐 Go）：宽 = 字号 + 左右 padding，高 = 候选行高，内容居中。
            // 命中区 = 整个矩形（与图标实际像素范围解耦），悬停在该矩形内即触发圆角高亮。
            let fpad = edges_or(&v.footer_bar.padding, [0.0, 6.0, 0.0, 6.0]);
            let arrow_w = footer_fs + fpad.l + fpad.r;
            // 触摸区高度：独立行=候选行高；并入编码所在行（独立栏 pager_will_inline 或内联行
            // pager_in_inline_row）=编码文字高(自适应)，使翻页栏不撑高那一行（消除有/无翻页栏
            // 时的抖动）并与编码垂直居中。
            let row_h = if pager_will_inline || pager_in_inline_row {
                preedit_fs.max(footer_fs)
            } else {
                // 横排时翻页按钮与候选同在一行，行高 = 文字行高 + 内边距，而行高 > 字号（字体
                // 上下留白）。按字号算会让按钮比行矮几个像素，居中取整后上下不对称，窗口内边距
                // 为 0 的主题（胶囊）悬停块贴着窗口边缘时一眼可见。行高取与候选文字同款样式量。
                // 竖排与网格展开态的翻页栏是独立一行，沿用字号算法，免得改动窗口高度。
                let line_h = if list_vertical || grid.is_some() {
                    text_fs
                } else {
                    // 行高取序号 / 文字 / 注释三者最高（与候选行排版一致）。
                    [
                        (text_fs, &v.text),
                        (node_fs(&v.comment), &v.comment),
                        (index_fs, &v.index),
                    ]
                    .into_iter()
                    .map(|(fs, n)| {
                        inp.text_renderer
                            .measure(
                                "国",
                                &Self::measure_style(fs, n.font_weight, n.font_family.as_deref()),
                            )
                            .height
                    })
                    .fold(text_fs, f32::max)
                };
                line_h + item_pad.t + item_pad.b
            };
            // 翻页箭头三档（顺序即主题编辑器「翻页符号」面板上公开写明的契约）：
            // prev/next_image（如 _base 的 chevron SVG + tint）> prev/next_char > 内置文字 ‹ ›。
            // 图配了 `ref = ""`（_qingfeng / msime 就这么清掉继承来的 chevron）时 arrow_icon
            // 返回 None，于是落到字符档。
            // 这里看到的图必定是主题**自己写的**：只从 base 继承来的那张已在合并层让位给本层
            // 写的字符（wind-theme `drop_inherited_arrow_images`），故此处无须再分辨来源。
            let prev_txt = arrow_char(&v.footer_bar.prev_char, "‹");
            let next_txt = arrow_char(&v.footer_bar.next_char, "›");
            let prev_icon = inp.arrow_icon(v.footer_bar.prev_image.as_ref(), prev_on);
            let next_icon = inp.arrow_icon(v.footer_bar.next_image.as_ref(), next_on);
            // 图标保持主题尺寸（footer_fs 方形），水平居中靠对称内边距撑到 arrow_w；
            // 垂直靠 cross(Center) 居中于 row_h。触摸盒 = arrow_w × row_h，与图标像素范围解耦。
            let icon_pad_x = ((arrow_w - footer_fs) * 0.5).max(0.0);
            let arrow =
                |icon: Option<ViewImage>, txt: &str, tag: i32, enabled: bool, hovered: bool| {
                    let mut node = match icon {
                        Some(vi) => View::container(Layout::Row)
                            .fixed_h(row_h)
                            .pad(Edges::xy(icon_pad_x, 0.0))
                            .child(
                                View::container(Layout::Row)
                                    .fixed_w(footer_fs)
                                    .fixed_h(footer_fs)
                                    .bg_image(vi),
                            ),
                        // 文字箭头启用色：优先 footer_bar.text_color（清风主题设为 text_hint → 细小淡 ‹›），
                        // 未配置则回退 accent（旧主题保持原样）。
                        //
                        // 宽度走 arrow_slot_w：arrow_w 这个基线是按**一个字**算的，而主题可以
                        // 把 prev_char 写成多字（schema 不限长），定死基线的话文本会盖到页码上
                        // 或被裁掉、命中区也仍是单字宽。取实测文本宽与基线的大者，单字符一侧
                        // 毫无变化（内置 ‹ › 及寻常符号都比基线窄）。
                        // 仍用 fixed_w 而非 min_w：宽度就此确定，与 pager_node_w() 的预算逐像素
                        // 对齐 —— 两处同源正是这个函数存在的理由。
                        None => View::leaf(txt, if enabled { arrow_on } else { disabled })
                            .font_size(footer_fs)
                            .fixed_w(inp.arrow_slot_w(txt, arrow_w, footer_fs, &v.footer_bar))
                            .fixed_h(row_h)
                            .text_align(Align::Center),
                    };
                    node = node.radius(item_radius).cross(Align::Center);
                    if enabled {
                        node = node.tag(tag); // 仅启用项参与命中
                        if hovered {
                            node = node.bg(hover_bg); // 圆角悬停高亮覆盖整个按钮矩形
                        }
                    }
                    node
                };
            Some(
                decorate_box(
                    View::container(Layout::Row)
                        .cross(Align::Center)
                        .margin(edges_or(&v.footer_bar.margin, [0.0, 0.0, 0.0, 8.0])),
                    &v.footer_bar,
                )
                .child(arrow(
                    prev_icon,
                    prev_txt,
                    TAG_PAGE_PREV,
                    prev_on,
                    inp.hover == TAG_PAGE_PREV,
                ))
                .child(
                    View::leaf(
                        if inp.page_number_visible() {
                            format!("{}/{}", inp.page, inp.total_pages)
                        } else {
                            String::new()
                        },
                        // 页码颜色用主题 footer_bar.color（如 svgtest 的亮红/暗粉），缺则回退 text_dim。
                        col(v.footer_bar.text_color, marker_c),
                    )
                    .font_size(footer_fs)
                    .font_weight(v.footer_bar.font_weight)
                    .font_family(v.footer_bar.font_family.clone()),
                )
                .child(arrow(
                    next_icon,
                    next_txt,
                    TAG_PAGE_NEXT,
                    next_on,
                    inp.hover == TAG_PAGE_NEXT,
                )),
            )
        } else {
            None
        };

        // 内联编码的"沉底"（swap_preedit_when_above 在首单元内联下的落点）：
        // 内联模式没有独立编码栏可参与末尾那段 band/list 交换装配，编码是 list 的首个子节点。
        // 横排下首单元 = 行首（最左），与上下无关，开关本就无意义 → 不参与；
        // 竖排下首单元 = 列首（最上），语义上等同于"编码栏在上"，此时开关必须生效 →
        // 把内联编码（及紧随其后的模式标记，二者共同构成独立栏模式下的"编码栏"内容）
        // 延迟到候选项之后再挂，得到与独立栏模式一致的"编码沉底贴光标"表现。
        let inline_preedit_bottom =
            swap_bands && list_vertical && inp.preedit_embedded && !inp.preedit.is_empty();
        // 沉底时延迟装配的内联节点，按原相对顺序在候选项之后追加。
        let mut inline_tail: Vec<View> = Vec::new();
        // 嵌入模式：编码作为候选行首单元内联（无独立背景，与候选间留白分隔），对齐 Go buildEmbeddedPreedit。
        // 横排右留白、竖排下留白（沉底时翻到上留白，否则会紧贴末个候选）。
        if inp.preedit_embedded && !inp.preedit.is_empty() {
            // 左缩进与独立条模式一致：取 preedit_bar.padding.left，避免编码贴窗口左缘。
            let pe_left = preedit_pad.l;
            let sep = if list_vertical || grid.is_some() {
                let gap = 6.0 * s;
                Edges {
                    l: pe_left,
                    t: if inline_preedit_bottom { gap } else { 0.0 },
                    b: if inline_preedit_bottom { 0.0 } else { gap },
                    ..Edges::default()
                }
            } else {
                Edges {
                    l: pe_left,
                    t: above_lift,
                    r: 16.0 * s,
                    ..Edges::default()
                }
            };
            let node = inp
                // 内联编码是 list 的子节点 ⇒ 在旋转包裹层里，直立态要逐格扶正。
                .preedit_view(preedit_fs, inline_preedit_budget_px, inp.upright);
            // 翻页栏并入这一行（`pager_in_inline_row`）：包一层撑满内容宽的 Row，spacer 把翻页栏
            // 顶到行末——与独立编码栏那条落点（band 内 spacer 右对齐）同构。
            // ⚠️ 先判开关再 `take()`：反过来写（`take().filter(…)`）会在开关关闭时把翻页栏取出来
            //    直接丢掉，末尾装配段再也拿不到它 —— 表现是翻页栏凭空消失，且只在关着开关时才犯。
            // 左缩进与行间距（sep）挂在**外层**：内外都挂会叠成两倍缩进，且行间距落在编码而非整行上。
            let node = if pager_in_inline_row && let Some(p) = pager.take() {
                View::container(Layout::Row)
                    .cross(Align::Center)
                    .fill_cross()
                    .child(node)
                    .child(View::spacer())
                    .child(p)
            } else {
                node
            };
            let node = node.margin(sep);
            if inline_preedit_bottom {
                inline_tail.push(node);
            } else {
                list = list.child(node);
            }
        }
        // 模式标记（拼/双/快/英/符 或全称）：紧随输入缓冲之后、候选之前内联显示。
        // 仅"无独立 preedit 栏"时在此（内联编码 candidate_inline / 内嵌应用 app_inline /
        // 栏模式但暂无 preedit）；有 preedit 栏时已置于栏行最后（见上）。
        // 颜色/背景由主题 mode_label 节点配置，与普通候选区分。横排右留白、竖排下留白。
        let preedit_bar_shown = !inp.preedit.is_empty() && !inp.preedit_embedded;
        if !inp.mode_label.is_empty() && !preedit_bar_shown {
            let ml_fs = node_fs(&v.mode_label);
            // 这个留白是**与后面的候选**拉开距离用的；一个候选都没有时（刚进临英/临拼、
            // 窗口里只有这个徽标）它就成了凭空多出来的边距，是 GH#127 那片空白的另一半。
            let trailing = if inp.candidates.is_empty() {
                0.0
            } else {
                12.0 * s
            };
            let sep = if list_vertical {
                // ⚠️ 这一支**不可能在无候选时走到**：`list_vertical` 自带 `&& !candidates
                // .is_empty()`（见其定义处的注释——无候选时强制 Row，免得徽标与占位行纵向
                // 堆叠把提示窗撑高）。所以竖排配置下的「只有徽标」那一帧走的是下面的 else，
                // 由 `trailing` 归零处理。别在这里再写一遍无候选分支，那是死代码。
                let gap = 6.0 * s;
                Edges {
                    t: if inline_preedit_bottom { gap } else { 0.0 },
                    b: if inline_preedit_bottom { 0.0 } else { gap },
                    ..Edges::default()
                }
            } else {
                Edges {
                    t: above_lift,
                    r: trailing,
                    ..Edges::default()
                }
            };
            // 主题为 mode_label 配了底色或边框 → 渲染为小徽标（圆角 + 内边距）以更醒目。
            // 与横排内嵌通路共用 decorate_mode_chip，两处装配保持一致。
            // 徽标同样逐格扶正（多为单字，与整块无差；「全称」档下才看得出来）。
            let ml_color = col(v.mode_label.text_color, [120, 120, 128, 255]);
            let ml_weight = v.mode_label.font_weight;
            let ml_family = v.mode_label.font_family.clone();
            let chip = decorate_mode_chip(
                inp.upright_text(inp.mode_label, None, |seg, _, _| {
                    View::leaf(seg.to_string(), ml_color)
                        .font_size(ml_fs)
                        .font_weight(ml_weight)
                        .font_family(ml_family.clone())
                })
                .margin(sep),
            );
            if inline_preedit_bottom {
                inline_tail.push(chip);
            } else {
                list = list.child(chip);
            }
        }
        // 无候选但有提示（模式徽标 / preedit）时：补一个与正常候选行等高的透明占位行，
        // 使提示窗口（如网址模式、临拼/临英刚进入）高度与有候选时及普通候选窗一致，
        // 避免窗口忽高忽低。占位行内边距/字号与候选行一致 → 测得同高，内容透明不可见。
        //
        // ★ `fixed_w(0.0)`：它的职责**只有撑高**，宽度一律不算。少了这一句，它会把
        // `item_pad` 左右（10+8）连同那个空格字符（约 0.6 字宽）一并计进行宽 —— 而这一行
        // 在横排里与模式徽标同处一行，于是「临时英文」四个字的提示框右边凭空多出近 30px
        // 空白（GH#127，用户原话「提示框右侧还有好多空白」）。`fixed_w` 覆盖的是含 padding
        // 的整宽、且不参与高度，正好只关掉宽度这一半（见 `View::measure`）。
        if inp.candidates.is_empty() {
            list = list.child(
                View::container(Layout::Row)
                    .cross(Align::Center)
                    .pad(item_pad)
                    .fixed_w(0.0)
                    .child(
                        View::leaf(" ".to_string(), [0, 0, 0, 0]).font_size(text_fs.max(index_fs)),
                    ),
            );
        }
        // ── 竖排最小行数（ui.candidate.min_rows）──
        // 候选不足时补足**与真实候选行同构**的透明占位行，使窗口高度不随候选数变化。
        //
        // 同构是硬要求，不能图省事复用上面那个「无候选提示行」（只有一个空格叶子）：真实行
        // 还带 item.margin 与序号节点（index_circle 时高 = index_fs × 1.5），缺了它们补出的
        // 行比真实行矮，窗口照样抖、只是抖得小一点——这种「修了但没修干净」最难被发现。
        let pad_rows = if list_vertical && inp.opts.min_rows > 0 {
            (inp.opts.min_rows as usize).saturating_sub(inp.candidates.len())
        } else {
            0
        };
        let placeholder_row = || {
            let mut ph = View::container(Layout::Row)
                .cross(Align::Center)
                .pad(item_pad)
                .margin(edges_or(&v.item.margin, [0.0; 4]))
                .fill_cross();
            // ⚠️ font_family / font_weight 必须照搬：真实后端的行高来自**该字族**的 line
            // metrics（DirectWrite 按 SetFontFamilyName 排版），不是字号的固定倍数。宋体约
            // 1.17 em、Microsoft YaHei UI 约 1.33 em，配了 [text] font_family 的主题下漏掉
            // 它就会让占位行矮一截，窗口照旧随候选数抖动。
            // mock 文本后端只按字号估算、不区分字族 ⇒ 等高性测试**测不出**这一项，靠
            // `placeholder_row_mirrors_real_row_text_attrs` 从结构上比对兜底。
            let mut idx = View::leaf(" ".to_string(), [0, 0, 0, 0])
                .font_size(index_fs)
                .font_weight(eff_weight(&v.index, &v.item, false, false))
                .font_family(v.index.font_family.clone())
                .pad(edges_or(&v.index.padding, [0.0; 4]))
                .margin(edges_or(&v.index.margin, [0.0; 4]));
            if index_circle {
                let d = (index_fs * 1.5).round();
                idx = idx.fixed_w(d).fixed_h(d);
            }
            ph = ph.child(idx);
            // 不设 tag（默认 -1）：占位行不进命中收集，鼠标划过或点击都不该有反应。
            let ph = ph.child(
                View::leaf(" ".to_string(), [0, 0, 0, 0])
                    .font_size(text_fs)
                    .font_weight(eff_weight(&v.text, &v.item, false, false))
                    .font_family(text_family.clone())
                    .pad(edges_or(&v.text.padding, [0.0; 4]))
                    .margin(edges_or(&v.text.margin, [0.0, 0.0, 0.0, 4.0])),
            );
            // 本页带上方条时真实行是 Column[上方条, 主行]，占位行同构。
            if has_above {
                stack_above(ph, above_ghost(false, false))
            } else {
                ph
            }
        };
        // 反转排列时占位行补在**顶部**：窗口上翻后底边贴光标、候选 1 在最下，空行若压在
        // 候选 1 下面会把它顶离光标，候选 1 的位置反而随候选数抖动——正是本功能要消除的。
        if reversed {
            for _ in 0..pad_rows {
                list = list.child(placeholder_row());
            }
        }

        // 展开态的格子先收集起来，装配完再按列数切成行（见循环后）。
        let mut grid_cells: Vec<View> = Vec::new();
        // `order` 与每候选的文字预算 `cand_text_budgets` 已在上方预扫阶段算好（k 与之同序）。
        for (k, (i, cand)) in order.into_iter().enumerate() {
            let is_sel = i == inp.selected;
            let is_hover = inp.hover >= 0 && inp.hover as usize == i;
            // 状态文字色（选中/悬停各自的色，回退基态）。
            let txt_color = eff_text(&v.text, text_color, is_sel, is_hover);
            let cmt_color = eff_text(&v.comment, comment_color, is_sel, is_hover);

            // 行内间距改由各子节点 margin 承载（与前端盒模型一致）：text.margin.left 默认 4dp
            // 作为序号↔文字间距；不再用容器 gap 借 text.margin.left。
            let mut item = View::container(Layout::Row)
                .cross(Align::Center)
                .pad(item_pad)
                .margin(edges_or(&v.item.margin, [0.0; 4]))
                .radius(item_radius)
                .tag(i as i32);
            // 竖排时撑满列宽（等于最宽候选的宽度），高亮背景宽度统一，与 Go 行为一致。
            if list_vertical {
                item = item.fill_cross();
            }
            // 网格：格宽不小于所在列宽，各行的格子上下对齐，高亮底色也一样宽。
            if grid.is_some() {
                item = item.min_w(grid_col_w[i % grid_cols]);
            }
            // 序号节点：no_index 项（如快捷加词提示行）完全跳过，避免空圆圈占位。
            if !cand.no_index {
                let marker = if cand.label.is_empty() {
                    (i + 1).to_string()
                } else {
                    cand.label.clone()
                };
                let idx_pad = edges_or(&v.index.padding, [0.0; 4]);
                let idx_margin = edges_or(&v.index.margin, [0.0; 4]);
                // 展开态只有数字键选得到的那几格显示序号（从高亮行起往下顺延，范围由协调器
                // 给出）。其余格子照样摆一个**透明**序号占住同样的宽度——直接不画的话，各行
                // 文字的起点就对不齐了。
                let idx_ghost = grid.is_some_and(|g| {
                    let s = g.index_start as usize;
                    !(s..s + g.index_count as usize).contains(&i)
                });
                let idx_color = if idx_ghost {
                    [0, 0, 0, 0]
                } else {
                    eff_text(&v.index, index_color, is_sel, is_hover)
                };
                // 圆圈样式 → 方形节点 + 真圆背景 + 居中数字。
                let mut idx_leaf = View::leaf(marker, idx_color)
                    .font_size(index_fs)
                    .font_weight(eff_weight(&v.index, &v.item, is_sel, is_hover))
                    .font_family(v.index.font_family.clone())
                    .pad(idx_pad)
                    .margin(idx_margin);
                if index_circle {
                    let d = (index_fs * 1.5).round();
                    idx_leaf = idx_leaf
                        .circle_bg(if idx_ghost {
                            [0, 0, 0, 0]
                        } else {
                            index_circle_bg
                        })
                        .fixed_w(d)
                        .fixed_h(d)
                        .text_align(Align::Center);
                }
                if let Some((bc, bw, br)) = eff_border(&v.index, is_sel, is_hover)
                    && !idx_ghost
                {
                    idx_leaf = idx_leaf.border(bc, bw).radius(br);
                }
                // 直立态：序号**整块**扶正，不逐字切——「10」于是横着读、位于列首，
                // 正是纵排里数字的常规排法（日文排版称 tate-chu-yoko）。
                // 逐字切会把它排成竖着的 1、0，看着像两个候选。
                item = item.child(if inp.upright {
                    View::rotated_ccw(idx_leaf)
                } else {
                    idx_leaf
                });
            }
            // no_index 行无序号节点：文字左边距归零，顶格不留序号间距（消除占位）。
            let text_margin = if cand.no_index {
                Edges::default()
            } else {
                edges_or(&v.text.margin, [0.0, 0.0, 0.0, 4.0])
            };
            // 文字预算来自上方的统一分配（竖排=整行独占，横排=water-filling 公平份额）。
            let display_text = inp.truncate_candidate_styled(
                &displays[k],
                &Self::measure_style(
                    text_fs,
                    eff_weight(&v.text, &v.item, is_sel, is_hover),
                    text_family.as_deref(),
                ),
                cand_text_budgets.get(k).copied().unwrap_or(min_text_w),
            );
            let text_weight = eff_weight(&v.text, &v.item, is_sel, is_hover);
            // 候选文字从「引擎给的原文」到「真正画上去的串」之间要过两道加工——按预算截断、
            // 选字族——**两道都不会失败，也都不留痕迹**。PUA 词库（蒙古文 Menksoft 等）出问题时，
            // 「少了几个字」与「用错了字体」在画面上长得一模一样，只有把两端的码位并排打出来
            // 才分得开。⚠️ 打码位而不是打字符：PUA 字符在日志文件里同样是不可见的空白。
            if tracing::enabled!(tracing::Level::DEBUG) {
                let cps = |s: &str| {
                    s.chars()
                        .map(|c| format!("{:04X}", c as u32))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                tracing::debug!(
                    "候选[{i}]{} 原文={} 显示={} 预算={:.1} 字族={} 字号={:.1} 字重={}",
                    if is_sel { "(选中)" } else { "" },
                    cps(&cand.text),
                    cps(display_text.as_str()),
                    cand_text_budgets.get(k).copied().unwrap_or(min_text_w),
                    text_family.as_deref().unwrap_or("<全局>"),
                    text_fs,
                    text_weight,
                );
            }
            // 直立态逐格扶正（见 `upright_text`）。装饰（底色/边框/内外边距）留在**外层
            // 容器**上，整段文字仍是一个整体，不会每个字各画一个药丸。
            // 模糊音提示段的颜色区间（无提示 = 空，叶子与改动前逐字段相同）；直立态逐格切时
            // 每格只拿自己那几段，同注释。
            let txt_state = text_state(is_sel, is_hover);
            let text_runs = crate::span_runs::pinyin_hint_runs(
                &v.text,
                &v.comment,
                txt_state,
                TEXT_FALLBACK,
                COMMENT_FALLBACK,
                crate::span_runs::candidate_backdrop(v, txt_state),
                &display_text,
            );
            let mut tleaf = inp
                .upright_text(display_text.as_str(), None, |seg, _, off| {
                    View::leaf(seg.to_string(), txt_color)
                        .font_size(text_fs)
                        .font_weight(text_weight)
                        .font_family(text_family.clone())
                        .color_runs(ColorRun::slice(&text_runs, off, off + seg.len()))
                })
                .pad(text_pad)
                .margin(text_margin);
            // 文字叶子的背景（底色/图/渐变）：此前只画边框，配了背景一律不生效，
            // 「文字药丸」这类样式做不出来。
            if let Some(c) = eff_bg(&v.text, is_sel, is_hover) {
                tleaf = tleaf.bg(c);
            }
            if let Some(vi) = inp.rv_image(v.text.bg_image.as_ref()) {
                tleaf = tleaf.bg_image(vi);
            }
            if let Some(g) = inp.rv_gradient(v.text.bg_gradient.as_ref()) {
                tleaf = tleaf.bg_gradient(g);
            }
            // 圆角独立于边框：只配底色不配边框时也要生效。
            let text_st = if is_sel {
                v.text.selected.as_deref()
            } else if is_hover {
                v.text.hover.as_deref()
            } else {
                None
            };
            if let Some(r) = text_st
                .and_then(|n| n.border_radius)
                .or(v.text.border_radius)
            {
                tleaf = tleaf.radius(r.resolve(s, 0.0));
            }
            if let Some((bc, bw, _)) = eff_border(&v.text, is_sel, is_hover) {
                tleaf = tleaf.border(bc, bw);
            }
            item = item.child(tleaf);
            // 注释（编码后缀/短语提示）：非空时在候选词右侧以注释样式内联显示。
            // 内/外边距完整消费：comment.padding 四边 + comment.margin 四边（左默认 6dp 兜底间距）。
            let mut comment = right_comment(cand, inp.rotated);
            // 竖排注释让步（分配规则见上方预扫）：超出预算时逐行截断加 `…`。
            if let Some(max_w) = cand_comment_budgets.get(k).copied().flatten() {
                let style = Self::measure_style(
                    comment_fs,
                    eff_weight(&v.comment, &v.item, is_sel, is_hover),
                    v.comment.font_family.as_deref(),
                );
                if let Some(cut) = inp.truncate_styled_lines(&comment, &style, max_w) {
                    comment = Cow::Owned(cut);
                }
            }
            if !comment.is_empty() {
                // 直立态同样逐格扶正：只让候选文字立起来、注释仍躺着的话，同一列里会有
                // 两种阅读方向。⚠️ 拉丁编码也照切——「英文横着读反而对」那条取舍已被真机
                // 推翻（旋转态是一切都转、自洽；直立态混排看不懂）。
                let cmt_weight = eff_weight(&v.comment, &v.item, is_sel, is_hover);
                let cmt_family = v.comment.font_family.clone();
                // 分段颜色：按当前主题与本候选的状态现求（悬停只有 UI 知道），等于正文色的丢弃。
                let cmt_runs = crate::span_runs::color_runs(
                    inp.theme,
                    &v.comment,
                    false,
                    text_state(is_sel, is_hover),
                    COMMENT_FALLBACK,
                    &comment,
                );
                let mut cleaf = inp
                    .upright_text(comment.as_str(), None, |seg, _, off| {
                        // 直立态逐格切叶子：每格只拿自己那几段颜色，平移到格内偏移。
                        View::leaf(seg.to_string(), cmt_color)
                            .font_size(comment_fs)
                            .font_weight(cmt_weight)
                            .font_family(cmt_family.clone())
                            .color_runs(ColorRun::slice(&cmt_runs, off, off + seg.len()))
                    })
                    .pad(edges_or(&v.comment.padding, [0.0; 4]))
                    .margin(edges_or(&v.comment.margin, [0.0, 0.0, 0.0, 6.0]));
                // 注释叶子背景同 text（「注释气泡」样式）。
                if let Some(c) = eff_bg(&v.comment, is_sel, is_hover) {
                    cleaf = cleaf.bg(c);
                }
                if let Some(vi) = inp.rv_image(v.comment.bg_image.as_ref()) {
                    cleaf = cleaf.bg_image(vi);
                }
                if let Some(g) = inp.rv_gradient(v.comment.bg_gradient.as_ref()) {
                    cleaf = cleaf.bg_gradient(g);
                }
                if let Some((bc, bw, br)) = eff_border(&v.comment, is_sel, is_hover) {
                    cleaf = cleaf.border(bc, bw).radius(br);
                }
                item = item.child(cleaf);
            }
            // 上方注释条：左缩进到候选文字字形起点；超出宽度上限（见 `above_cols`）时截断加 `…`，
            // 截的是显示副本，颜色区间随 `cut_with_mark` 一起裁。
            if has_above {
                let above = if cand.comment_above.is_empty() {
                    above_ghost(is_sel, is_hover)
                } else {
                    let (indent, max_w) = above_cols[k];
                    let weight = eff_weight(&v.comment_above, &v.item, is_sel, is_hover);
                    let src = &cand.comment_above;
                    let shown = inp.truncate_text_for_width(
                        src.as_str(),
                        &Self::measure_style(above_fs, weight, above_family.as_deref()),
                        max_w,
                    );
                    let text = if shown == src.as_str() {
                        src.clone()
                    } else {
                        src.cut_with_mark(shown.len() - '…'.len_utf8(), "…")
                    };
                    let runs = crate::span_runs::color_runs(
                        inp.theme,
                        &v.comment_above,
                        false,
                        text_state(is_sel, is_hover),
                        COMMENT_FALLBACK,
                        &text,
                    );
                    let base = col(v.comment_above.text_color, COMMENT_FALLBACK);
                    let mut aleaf = View::leaf(
                        text.into_string(),
                        eff_text(&v.comment_above, base, is_sel, is_hover),
                    )
                    .font_size(above_fs)
                    .font_weight(weight)
                    .font_family(above_family.clone())
                    .color_runs(runs)
                    .pad(above_pad)
                    .margin(Edges {
                        l: above_margin.l + indent,
                        ..above_margin
                    });
                    // 装饰与右侧注释叶子同一套消费（底色 / 背景图 / 渐变 / 边框，含选中悬停态）。
                    // 占位叶子不画这些：边框不占布局尺寸（`View::measure` 不计），不影响等高。
                    if let Some(c) = eff_bg(&v.comment_above, is_sel, is_hover) {
                        aleaf = aleaf.bg(c);
                    }
                    if let Some(vi) = inp.rv_image(v.comment_above.bg_image.as_ref()) {
                        aleaf = aleaf.bg_image(vi);
                    }
                    if let Some(g) = inp.rv_gradient(v.comment_above.bg_gradient.as_ref()) {
                        aleaf = aleaf.bg_gradient(g);
                    }
                    if let Some((bc, bw, br)) = eff_border(&v.comment_above, is_sel, is_hover) {
                        aleaf = aleaf.border(bc, bw).radius(br);
                    }
                    aleaf
                };
                item = stack_above(item, above);
            }
            // 候选项基态底色：此前只在选中/悬停时调 .bg()，`[item] background = "…"` 从不生效，
            // 而同级的背景图/渐变基态是读的（见下），三者本该同级。选中/悬停底色随后覆盖。
            if let Some(c) = v.item.bg_color {
                item = item.bg(c);
            }
            // 选中底色优先于悬停底色（两者独立：选中=空格上屏目标，悬停=鼠标提示）
            if is_sel {
                item = item.bg(sel_bg);
                if let Some(bar) = accent_bar {
                    item = item.left_bar(bar);
                }
            } else if is_hover {
                item = item.bg(hover_bg);
            }
            // 候选项背景图：选中态优先用 selected patch 的图（如 jidian 的 sel.png），否则用 base。
            let item_img = if is_sel {
                inp.rv_image(v.item.selected.as_ref().and_then(|n| n.bg_image.as_ref()))
                    .or_else(|| inp.rv_image(v.item.bg_image.as_ref()))
            } else {
                inp.rv_image(v.item.bg_image.as_ref())
            };
            if let Some(vi) = item_img {
                item = item.bg_image(vi);
            }
            // 候选项背景渐变：选中态优先用 selected patch 的渐变，否则用 base。
            let item_grad = if is_sel {
                inp.rv_gradient(
                    v.item
                        .selected
                        .as_ref()
                        .and_then(|n| n.bg_gradient.as_ref()),
                )
                .or_else(|| inp.rv_gradient(v.item.bg_gradient.as_ref()))
            } else {
                inp.rv_gradient(v.item.bg_gradient.as_ref())
            };
            if let Some(g) = item_grad {
                item = item.bg_gradient(g);
            }
            // 候选项边框（含 selected/hover 换色换宽）。eff_border 此前只用在
            // index/text/comment 三个叶子上，item 容器自己从没画过边框——
            // 主题里写的 [item] border / [item.selected] border 一律不生效。
            // 只取色与宽：圆角沿用上面按 item_radius 设好的值，避免 eff_border 的
            // 0.0 兜底把未配 border.radius 的候选项圆角抹平。
            if let Some((bc, bw, _)) = eff_border(&v.item, is_sel, is_hover) {
                item = item.border(bc, bw);
            }
            if grid.is_some() {
                grid_cells.push(item);
            } else {
                list = list.child(item);
            }
        }
        // 网格装配：每 `cols` 格成一行。末屏行数不足时补与真实行同构的占位行（同竖排
        // `min_rows` 那套），滚到末尾窗口也不变矮——窗口高度一变，上翻定位时整个窗口就跳。
        // 展开态的分类标签栏，挂到表格**下方**与翻页栏同一行（见装配段）。放在上方会把首行
        // 往下推，展开就不像「原地往下长出来」了。
        let mut grid_tab_bar: Option<View> = None;
        if let Some(g) = grid {
            // 分类标签栏（P3）：当前分类画选中底色。命中 tag 落在 `GRID_TAB_TAG_BASE` 段，
            // 点击经候选点选那条路回到协调器（宿主原样回传正数下标）。展开期间它恒在，
            // 窗口高度不因切分类而变。
            if g.tabs != 0 {
                let dim_c = t.color("text_dim", [140, 140, 145, 255]);
                let mut bar = View::container(Layout::Row)
                    .gap(4.0 * s)
                    .cross(Align::Center);
                for tab in wind_ui_types::GridTab::ALL {
                    if g.tabs & tab.bit() == 0 {
                        continue;
                    }
                    let active = tab as u8 == g.active_tab;
                    let color = if active {
                        eff_text(&v.text, text_color, true, false)
                    } else {
                        dim_c
                    };
                    let mut chip = View::leaf(tab.label().to_string(), color)
                        .font_size(comment_fs)
                        .font_family(text_family.clone())
                        .pad(Edges::xy(8.0 * s, 2.0 * s))
                        .radius(item_radius)
                        .tag(wind_ui_types::GRID_TAB_TAG_BASE + tab as i32);
                    if active {
                        chip = chip.bg(sel_bg);
                    }
                    bar = bar.child(chip);
                }
                grid_tab_bar = Some(bar);
            }
            let mut cells = grid_cells.into_iter().peekable();
            let mut shown = 0usize;
            while cells.peek().is_some() {
                let mut row = View::container(Layout::Row)
                    .gap(box_gap)
                    .cross(Align::Center);
                for cell in cells.by_ref().take(grid_cols) {
                    row = row.child(cell);
                }
                list = list.child(row);
                shown += 1;
            }
            for _ in shown..g.rows as usize {
                list = list.child(placeholder_row());
            }
        }
        // 正常顺序：占位行补在候选之后（窗口下方留白，候选 1 恒在顶部）。
        if !reversed {
            for _ in 0..pad_rows {
                list = list.child(placeholder_row());
            }
        }
        // 内联编码沉底（见 inline_preedit_bottom）：候选项装配完毕后按原顺序追加编码与模式标记。
        // 竖排未并入的翻页栏另在末尾装配段随 swap 翻到顶部，故此处追加即为窗口最底部。
        for node in inline_tail {
            list = list.child(node);
        }

        // ── 装配 ──
        // 皮肤分槽位布局：配了翻页槽就先把翻页栏拿走，不参与下面的三选一。
        let free_slots = self.skin_geom.as_ref().and_then(|g| g.slots());
        let footer_part = match free_slots {
            Some(slots) if slots.footer.is_some() => pager.take(),
            _ => None,
        };
        // 翻页栏归属（按优先级三选一）：
        //   1) pager_will_inline → 追加到编码栏 band 右端（随编码区一同装配，swap 时自动沉底贴光标）
        //   2) 横排 → 并入候选行尾（原行为）
        //   3) 竖排 → 候选区底部独立行（原行为）
        if pager_will_inline {
            if let (Some(band), Some(p)) = (preedit_band.take(), pager.take()) {
                preedit_band = Some(band.child(p));
            }
        } else if !list_vertical
            && grid.is_none()
            && let Some(mut p) = pager.take()
        {
            if has_above {
                p.margin.t += above_lift; // 与主行对齐，见 `above_lift`
            }
            list = list.child(p);
        }
        // 竖排未并入的翻页栏：候选区独立行（与 list 同属候选区，随 swap 一起移动）。
        // 独立行按主题 behavior.pager_align（left/center/right，默认 center）水平对齐：包一层
        // fill_cross 的 Row 用 spacer 顶位。inline 情形已在编码栏内右对齐，不经此。
        // ⚠️ 判据是 `list_vertical` 不是 `inp.vertical`：旋转态的列表在局部空间里也是竖排，
        // 翻页栏同样该独立成行（它加在 `root` 上、在旋转包裹之外，故屏幕上仍是横的一条）。
        // 网格同样走这一支：翻页栏挂在表格下方独立成行，不去抢某一行的宽度。
        let vertical_bottom_pager = if grid.is_some() {
            // 展开态：分类标签与翻页箭头同一行、整体居中（`[标签栏]  ‹ ›`）。不跟主题的
            // `pager_align` 走：这一行在表格正下方，居中才与上面对称的格子对得上。
            let pager = pager.take();
            (grid_tab_bar.is_some() || pager.is_some()).then(|| {
                let mut row = View::container(Layout::Row)
                    .fill_cross()
                    .cross(Align::Center)
                    .gap(12.0 * s)
                    .child(View::spacer());
                if let Some(bar) = grid_tab_bar.take() {
                    row = row.child(bar);
                }
                if let Some(p) = pager {
                    row = row.child(p);
                }
                row.child(View::spacer())
            })
        } else if list_vertical {
            pager.take().map(|p| {
                let row = View::container(Layout::Row)
                    .fill_cross()
                    .cross(Align::Center);
                match inp.theme.behavior.pager_align.as_str() {
                    "left" => row.child(p),
                    "right" => row.child(View::spacer()).child(p),
                    _ => row.child(View::spacer()).child(p).child(View::spacer()),
                }
            })
        } else {
            None
        };

        // 旋转包裹**只套候选列表**：编码栏、模式标记、翻页栏都留在屏幕坐标系里横着排
        // （参考图里 "eho" 那一行就是横的）。包裹层是裸的，装饰全在 `list` 上跟着一起转。
        //
        // ⚠️ 上一句只对**独立**编码栏成立。`preedit_display = "candidate_inline"` 时编码是
        // `list` 的子节点（见上方 `inline_tail`），会跟着转成竖的。出厂是 `app_inline`
        // （编码由宿主自绘），故不是默认路径；蒙文用户若开了内联编码需要另行处理。
        // ⚠️ 判据必须与 `list_vertical` 同源，**不能只判 `inp.rotated`**：无候选时列表被
        // 刻意退化成 Row（徽标 + 等高占位，见 `list_vertical` 上方注释），此时再套旋转
        // 就会把「刚进临英/临拼、只有模式徽标」的那一帧转成又窄又高的一竖条、徽标横躺。
        // 那一帧真会显示（`render_frame` 在 mode_label 非空时就显示窗口）。
        let list = if list_vertical && inp.rotated {
            View::rotated_cw(list)
        } else {
            list
        };

        // 皮肤分槽位布局：各块交给皮肤层按槽位摆放，根容器只是一个按窗体尺寸撑开的空盒
        // （见 `skin_geom::CandidateScene::skin_layout`）。槽位由皮肤决定，不随上翻交换。
        if let Some(slots) = free_slots {
            let mut cand = View::container(Layout::Column).child(list);
            if let Some(p) = vertical_bottom_pager {
                cand = cand.child(p);
            }
            let mut parts = Vec::with_capacity(3);
            match (slots.preedit.is_some(), preedit_band) {
                (true, Some(band)) => parts.push((skin_geom::SlotKind::Preedit, band)),
                (_, band) => {
                    // 没配编码槽：编码栏照旧叠在候选上方，一起进候选槽。
                    let mut col = View::container(Layout::Column);
                    if let Some(band) = band {
                        col = col.child(band);
                        if let Some(sep) = preedit_sep {
                            col = col.child(sep);
                        }
                    }
                    cand = col.child(cand);
                }
            }
            parts.push((skin_geom::SlotKind::Candidates, cand));
            if let Some(p) = footer_part {
                parts.push((skin_geom::SlotKind::Footer, p));
            }
            *self.free_parts.borrow_mut() = parts;
            return root;
        }
        // 编码区（band + 分隔线）与候选区（list + 竖排底部翻页栏）按 swap_bands 决定上下顺序：
        //   false（正常）：编码区在上、候选区在下 → [band][sep][候选区]
        //   true （上翻）：候选区在上、编码区在下 → [候选区][sep][band]（编码沉底贴光标）
        // 分隔线始终位于编码栏"朝向候选区"的一侧，语义（分隔编码/候选）保持不变。
        if swap_bands {
            // 完整上下镜像：正常 [编码][候选][翻页] → 镜像 [翻页][候选][编码]。
            // 竖排底部翻页栏一并翻到顶部，否则会夹在候选与编码之间，视觉不自然。
            if let Some(p) = vertical_bottom_pager {
                root = root.child(p);
            }
            root = root.child(list);
            if let Some(sep) = preedit_sep {
                root = root.child(sep);
            }
            if let Some(band) = preedit_band {
                root = root.child(band);
            }
        } else {
            if let Some(band) = preedit_band {
                root = root.child(band);
            }
            if let Some(sep) = preedit_sep {
                root = root.child(sep);
            }
            root = root.child(list);
            if let Some(p) = vertical_bottom_pager {
                root = root.child(p);
            }
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主题配的翻页字符必须原样落到渲染，只有「一个字符都没配」才回退内置箭头。
    ///
    /// 这条以前是断的：渲染侧把 ‹ › 写死，`footer_bar.prev_char` 解析到 RvNode 后无人消费，
    /// 主题里写什么都被画回内置箭头（而主题编辑器的预览一直照配置画，两边从来没对齐过）。
    /// 空白不算未配——`prev_char = " "` 是「这里留白」的显式意图，被当成未配置就又变回 ‹ ›，
    /// 正是同一个 bug 的另一副面孔。
    #[test]
    fn arrow_char_falls_back_only_when_unset() {
        assert_eq!(arrow_char("", "‹"), "‹", "未配 → 内置箭头");
        assert_eq!(arrow_char("$", "‹"), "$", "配了 ASCII 符号 → 原样");
        assert_eq!(arrow_char(")", "›"), ")", "配了 ASCII 符号 → 原样");
        assert_eq!(arrow_char("◀", "‹"), "◀", "配了多字节字符 → 原样");
        // 多字原样传递；按钮宽度按文本撑开，见 pager_inline_tests 的 multi_char_arrow_*
        assert_eq!(arrow_char("上页", "‹"), "上页", "配了多字 → 原样");
        assert_eq!(arrow_char(" ", "‹"), " ", "留白是显式意图，不回退");
    }

    /// 候选文本的空白控制符须显示为可见符号，且**不得改动其他字符**。
    ///
    /// 这只是显示投影——`Candidate::text` 原文不动，上屏走的仍是真换行（见
    /// [`visible_whitespace`] 的文档）。故本测试只锁「显示成什么样」，
    /// 不能被误读成「上屏文本也变了」。
    #[test]
    fn whitespace_becomes_visible_glyphs() {
        // 无控制符：原样借用，零分配
        assert!(matches!(visible_whitespace("你好"), Cow::Borrowed("你好")));

        assert_eq!(visible_whitespace("甲\n乙"), "甲↵乙");
        assert_eq!(visible_whitespace("丙\t丁"), "丙⇥丁");
        // CRLF 是**一个**换行，只出一个符号
        assert_eq!(visible_whitespace("戊\r\n己"), "戊↵己");
        // 孤立 CR 同样算换行
        assert_eq!(visible_whitespace("庚\r辛"), "庚↵辛");
        // 混合 + 连续
        assert_eq!(visible_whitespace("a\n\tb"), "a↵⇥b");
        // 普通空格不动——它本来就看得见，替换只会平添噪声
        assert_eq!(visible_whitespace("甲 乙"), "甲 乙");
    }
}
