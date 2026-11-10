//! 预览 API 本体（与 JS 无关，原生目标上用 mock 画布单测）：主题文本 + 资源 → 一帧候选窗 RGBA。
//!
//! 渲染走与设置端预览（wind-ui `candidate_window/preview.rs`）同一条路：
//! [`effective_theme`] 叠外观覆盖 → [`render_candidate_within`]。动画由调用方给时刻（`t_ms`），
//! 事件经 [`Preview::signal`] 喂装饰状态机。
//!
//! 参数与主题都来自编辑器（主题可能是从市场导入的不可信文本），一律设上限：参数超限直接报错，
//! 生效字号超限报错，排版出来的帧超过 [`MAX_SIDE`] / [`MAX_PIXELS`] 在分配缓冲之前报错。
//! 本期不支持旋转（rotated）与直立（upright）排版、不支持展开网格。

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wind_scene::assets::MemoryAssets;
use wind_scene::candidate::{
    AnimClock, CandidateScene, CandidateSceneInput, CandidateSceneOptions, ThemeOverrides,
    effective_theme, render_candidate_within,
};
use wind_scene::view::PaintCaches;
use wind_theme::Resolved;
use wind_theme::skin::DecorEvent;
use wind_ui_types::CandidateItem;

use crate::canvas::{Canvas, CanvasText};

/// 帧缓冲单边上限（设备像素）。
pub const MAX_SIDE: u32 = 4096;
/// 帧缓冲像素总数上限（RGBA 约 32 MB）。
pub const MAX_PIXELS: u64 = 4096 * 2048;
/// 生效候选字号上限（逻辑像素；`font_size` 参数与主题 `behavior.font_size` 都受它管）。
pub const MAX_FONT_SIZE: f32 = 200.0;
/// DPI 缩放上限。
pub const MAX_SCALE: f32 = 8.0;
/// 自定义候选条数上限。
pub const MAX_CANDIDATES: usize = 50;
/// 单条文字（候选、注释、标签、编码、字族名）的字符数上限。
pub const MAX_TEXT_CHARS: usize = 256;
/// `t_ms` 钳到这里（约 31 年）。
pub const MAX_T_MS: f64 = 1e12;
/// 页码与总页数上限。
pub const MAX_PAGES: usize = 9999;
/// 预览当作的屏幕边长（逻辑像素）：设备像素 = 它 × `scale`。按逻辑像素定，换缩放时文字预算
/// 不变、截不截断与缩放无关；scale > 2 时长内容会撞 [`MAX_SIDE`]，报 [`FRAME_TOO_LARGE`]，
/// 由调用方降缩放重画。
pub const SCREEN_DP: u32 = 2048;
/// 帧超过 [`MAX_SIDE`] / [`MAX_PIXELS`] 时报错的前缀（调用方据此降缩放重试）。
pub const FRAME_TOO_LARGE: &str = "帧过大：";

/// 一条自定义候选。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CandidateOpt {
    pub text: String,
    pub comment: String,
    /// 序号标签；空 = 主题槽位，再空按数字。
    pub label: String,
    /// 上方注释条文字（`ui.candidate.comment_above` 开时的上段）；空 = 无上段。
    pub comment_above: String,
    /// 正确拼音提示段在 `text` 里的字节区间 `[起, 止)`（模糊音「中(zhong)文」里的 `(zhong)`）。
    pub hint: Option<[usize; 2]>,
}

/// 候选正文：有提示区间就切成「前 / 提示 / 后」三段，提示段带拼音提示角色。
fn candidate_styled(c: &CandidateOpt) -> Result<wind_ui_types::StyledText, String> {
    let mut out = wind_ui_types::StyledText::new();
    let Some([a, b]) = c.hint else {
        return Ok(out);
    };
    let t = &c.text;
    if !(a <= b && b <= t.len() && t.is_char_boundary(a) && t.is_char_boundary(b)) {
        return Err(format!("拼音提示区间 [{a}, {b}) 不在 {t:?} 的字符边界上"));
    }
    let plain = wind_ui_types::SpanStyle::default();
    out.push(&t[..a], &plain);
    out.push(
        &t[a..b],
        &wind_ui_types::SpanStyle {
            role: Some(wind_theme::PINYIN_HINT_ROLE),
            ..Default::default()
        },
    );
    out.push(&t[b..], &plain);
    Ok(out)
}

/// `render` / `signal` 的参数（JSON，字段全可省）。
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RenderOpts {
    /// 暗色变体。
    pub dark: bool,
    /// 竖排（不旋转、不直立）。
    pub vertical: bool,
    /// 编码嵌入（无独立编码栏）。
    pub embedded: bool,
    /// 候选窗在光标上方（上方专属布局）。
    pub above: bool,
    /// DPI 缩放。
    pub scale: f32,
    /// 示例候选条数（`candidates` 为空时用；钳到 1..=10）。
    pub per_page: usize,
    /// 自定义候选；空 = 与设置端预览同一套示例。
    pub candidates: Vec<CandidateOpt>,
    /// 编码；None = 示例编码 `nihao`。
    pub preedit: Option<String>,
    /// 光标在编码里的字节偏移；None = 末尾。
    pub caret: Option<usize>,
    pub selected: usize,
    /// 悬停项（页内下标），-1 = 无。
    pub hover: i32,
    pub page: usize,
    pub total_pages: usize,
    /// 模式标签（如「中」）；空 = 不显示。
    pub mode_label: String,
    /// 动画时间轴时刻（毫秒，调用方的时间原点）。
    pub t_ms: f64,
    /// 播不播动画；false = 装饰停在初始状态首帧（设置端预览的形态）。
    pub animate: bool,
    /// 采用主题阴影（`ui.candidate.shadow`）。
    pub shadow: bool,
    /// 画软件阴影（Windows 实机形态）；false = macOS / Linux 外部宿主的形态（无扩边）。
    pub soft_shadow: bool,
    /// 用户外观字体（`ui.font.family`）；空 = 跟随主题，主题也没写就是默认字族栈。
    pub font_family: String,
    /// 用户全局字重（`ui.font.weight`）；0 = 不指定。
    pub font_weight: i32,
    /// 候选字号覆盖（`ui.candidate.font_size`，逻辑像素）；0 = 跟随主题。
    pub font_size: f32,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            dark: false,
            vertical: false,
            embedded: false,
            above: false,
            scale: 1.0,
            // `ui.candidate.per_page` 的出厂值。
            per_page: 7,
            candidates: Vec::new(),
            preedit: None,
            caret: None,
            selected: 0,
            hover: -1,
            page: 1,
            // 同设置端预览：翻页栏显示「1/3」。
            total_pages: 3,
            mode_label: String::new(),
            t_ms: 0.0,
            animate: false,
            shadow: true,
            soft_shadow: true,
            font_family: String::new(),
            font_weight: 0,
            font_size: 0.0,
        }
    }
}

/// 一帧：直通 alpha 的 RGBA（`width × height × 4`，可直接作 `ImageData`）。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// 下一次换帧时刻（毫秒，同 `t_ms` 的时间轴）；None = 画面静止，不必再画。
    pub next_ms: Option<f64>,
    /// 各节点的矩形（先序），供编辑器点选与选中描边。
    pub nodes: Vec<NodeRect>,
    /// 窗口宽过了预览屏（[`SCREEN_DP`]），按实机的屏幕安全宽钳窄了。
    pub clamped: bool,
    /// 皮肤几何；None = 本帧没启用皮肤（没配、变体回退普通窗口、或引擎回退）。
    pub skin: Option<SkinInfo>,
    /// 本次调用期间引擎的 `skin:` 告警原文（见 [`crate::warnings`]）。
    pub warnings: Vec<String>,
}

/// 矩形（设备像素，与 `rgba` 同坐标）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RectInfo {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl From<wind_scene::view::Rect> for RectInfo {
    fn from(r: wind_scene::view::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

/// 一段切片映射。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AxisSegInfo {
    /// `frame.image` 的源像素 `[a, b)`（与 `stretch_x/y` 同坐标）。
    pub src: [f32; 2],
    /// 设备像素 `[a, b)`（缓冲坐标）。
    pub dst: [f32; 2],
    /// `fixed` / `stretch` / `tile` / `round`。
    pub kind: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SlotInfo {
    /// `preedit` / `candidates` / `footer`。
    pub name: &'static str,
    /// 锚在哪个盒上：`frame` / `content`。
    #[serde(rename = "box")]
    pub on: &'static str,
    pub rect: RectInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipInfo {
    /// `states` 的名字、`on.<事件>`（一次性片段）；null = 整个装饰文件。
    pub state: Option<String>,
    /// 片段内第几帧。
    pub frame: usize,
    /// 此刻画的是文件的第几个原始帧（`inspectImage` 的帧号）；null = 有帧解码失败、对不上。
    pub raw: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecorInfo {
    /// 在求值后变体 `decor`（按 z 稳定排序）里的下标，即 `click:<N>` 用的那个；渲染时求值失败的
    /// 装饰不出现，下标可能不连续。
    pub index: usize,
    /// 在 TOML `[[skin.<变体>.decor]]` 里的原始下标（编辑器按它对回模型）。指的是**定义这张变体表的
    /// 那份 TOML**：变体继承自 base（子主题没写这个变体键）时是 base 里的下标。
    pub src_index: usize,
    pub rect: RectInfo,
    pub z: i32,
    /// `drag` / `block` / `pass`。
    pub hit: &'static str,
    pub clip: ClipInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaretInfo {
    /// 下方 / 上方时对齐光标的点（设备像素，缓冲坐标）。
    pub below: [f32; 2],
    pub above: [f32; 2],
}

/// 帧回执的 `skin` 段。矩形、点、`dst` 一律设备像素、与 `rgba` / `nodes` 同坐标（含装饰外扩与
/// 阴影扩边；÷ `scale` 得逻辑像素）；`axisX/Y.src` 是窗体图源像素；`k` 把主坐标（`skin.density`
/// 源像素：`content`、槽位 / 装饰 / 对齐点的 `offset`）换成设备像素。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkinInfo {
    /// 实际所用的变体（嵌入没配时是回退到的非嵌入变体）。
    pub variant: &'static str,
    pub k: f32,
    /// 设备像素 / dp（= `k × skin.density`）。
    pub render_scale: f32,
    /// 所选的窗体源图引用（`frame.image` 或 srcset 的某一份）。
    pub frame_source: String,
    pub min_size: [f32; 2],
    pub frame: RectInfo,
    pub content: RectInfo,
    pub axis_x: Vec<AxisSegInfo>,
    pub axis_y: Vec<AxisSegInfo>,
    /// 分槽位布局各块；stack 布局为空。
    pub slots: Vec<SlotInfo>,
    pub decor: Vec<DecorInfo>,
    pub caret: CaretInfo,
}

fn axis_info(segs: &[wind_scene::candidate::SkinAxisSeg]) -> Vec<AxisSegInfo> {
    use wind_theme::skin::BandFill;
    segs.iter()
        .map(|s| AxisSegInfo {
            src: [s.src.0, s.src.1],
            dst: [s.dst.0, s.dst.1],
            kind: match s.fill {
                None => "fixed",
                Some(BandFill::Stretch) => "stretch",
                Some(BandFill::Tile) => "tile",
                Some(BandFill::Round) => "round",
            },
        })
        .collect()
}

impl From<wind_scene::candidate::SkinSnapshot> for SkinInfo {
    fn from(s: wind_scene::candidate::SkinSnapshot) -> Self {
        use wind_scene::candidate::SlotKind;
        use wind_theme::skin::DecorHit;
        Self {
            variant: s.variant,
            k: s.k,
            render_scale: s.render_scale,
            frame_source: s.frame_source,
            min_size: [s.min_size.0, s.min_size.1],
            frame: s.frame.into(),
            content: s.content.into(),
            axis_x: axis_info(&s.axis_x),
            axis_y: axis_info(&s.axis_y),
            slots: s
                .slots
                .iter()
                .map(|p| SlotInfo {
                    name: match p.kind {
                        SlotKind::Preedit => "preedit",
                        SlotKind::Candidates => "candidates",
                        SlotKind::Footer => "footer",
                    },
                    on: if p.on_content { "content" } else { "frame" },
                    rect: p.rect.into(),
                })
                .collect(),
            decor: s
                .decor
                .into_iter()
                .map(|d| DecorInfo {
                    index: d.index,
                    src_index: d.src_index,
                    rect: d.rect.into(),
                    z: d.z,
                    hit: match d.hit {
                        DecorHit::Drag => "drag",
                        DecorHit::Block => "block",
                        DecorHit::Pass => "pass",
                    },
                    clip: ClipInfo {
                        state: d.clip,
                        frame: d.frame,
                        raw: d.raw,
                    },
                })
                .collect(),
            caret: CaretInfo {
                below: [s.caret_below.0, s.caret_below.1],
                above: [s.caret_above.0, s.caret_above.1],
            },
        }
    }
}

/// 一个节点在帧里的矩形：**设备像素**（与 `rgba` 同一坐标系，含阴影扩边；除以 `scale` 得逻辑
/// 像素）。`key` 是主题 `views.candidate` 下的节点名（`window` / `item` / `index` …）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeRect {
    pub key: &'static str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct Preview<C> {
    text: CanvasText<C>,
    /// 当前主题的 `[亮, 暗]` 求值结果。
    themes: [Resolved; 2],
    scene: CandidateScene,
    scene_opts: CandidateSceneOptions,
    /// 本实例的资源表（内置资产 + 调用方资源）与代号；每次排版 / 绘制前登记成本线程的当前表。
    assets: Arc<MemoryAssets>,
    assets_gen: u64,
    /// 调用方资源的指纹（路径 + 长度 + 内容哈希），没变就沿用代号与缓存，不重新解码。
    assets_fp: u64,
    /// 本实例的图片 / 阴影缓存：同页别的实例换主题、清缓存都不波及它。
    caches: PaintCaches,
    /// 下一帧让命中的图集 / 动画缓存重放建它时的告警（`set_theme` 之后的首帧）：资源没变时缓存
    /// 跨 `set_theme` 沿用，那些告警不会再报。
    replay_warnings: bool,
    /// 上一帧已画出（装饰状态机视窗口为显示中）。
    visible: bool,
    /// 状态机按哪种动画开关建的。
    anim_allowed: bool,
    /// 上一帧的参数（`signal` 用它搭场景输入）。
    last: RenderOpts,
}

/// `t_ms` → 时间轴时长：负数、NaN 当 0，钳到 [`MAX_T_MS`]。
fn duration_ms(t_ms: f64) -> Duration {
    let ms = if t_ms.is_finite() {
        t_ms.clamp(0.0, MAX_T_MS)
    } else {
        0.0
    };
    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(Duration::ZERO)
}

/// 编辑器预览的 base 只能是编进产物的内置主题。
fn builtin_base(id: &str) -> anyhow::Result<String> {
    crate::builtin::theme_text(id).ok_or_else(|| {
        let ids: Vec<_> = crate::builtin::theme_ids().collect();
        anyhow::anyhow!(
            "base 主题 {id:?} 不是内置主题：编辑器预览只认内置主题作 base（{}）",
            ids.join(" / ")
        )
    })
}

/// 主题文本 → `[亮, 暗]`。
fn resolve_text(toml: &str, id: &str) -> Result<[Resolved; 2], String> {
    let t = wind_theme::load_typed_text(toml, id, &builtin_base).map_err(|e| format!("{e:#}"))?;
    Ok([
        wind_theme::resolve(&t, false, &[]),
        wind_theme::resolve(&t, true, &[]),
    ])
}

/// 调用方资源的指纹：按路径排序后的（路径, 长度, 内容哈希）。
fn assets_fingerprint(assets: &[(String, Vec<u8>)]) -> u64 {
    let mut v: Vec<&(String, Vec<u8>)> = assets.iter().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (p, b) in v {
        p.hash(&mut h);
        b.len().hash(&mut h);
        b.hash(&mut h);
    }
    h.finish()
}

/// 内置资产 + 调用方资源（同名覆盖内置）。
fn build_assets(assets: Vec<(String, Vec<u8>)>) -> MemoryAssets {
    let mut m = MemoryAssets::new();
    crate::builtin::add_assets(&mut m);
    for (path, bytes) in assets {
        m.insert(&path, bytes);
    }
    m
}

/// 事件名 → 装饰事件与目标装饰：`typing` / `commit` / `idle_5s` …，`click` 给全部装饰，
/// `click:2` 只给第 2 块。
fn parse_signal(name: &str) -> Option<(DecorEvent, Option<usize>)> {
    if let Some(i) = name.strip_prefix("click:") {
        return Some((DecorEvent::Click, Some(i.parse().ok()?)));
    }
    Some((DecorEvent::parse(name)?, None))
}

/// 一段文字不超过 [`MAX_TEXT_CHARS`] 个字符。
fn check_len(what: &str, s: &str) -> Result<(), String> {
    let n = s.chars().count();
    if n > MAX_TEXT_CHARS {
        return Err(format!("{what}过长：{n} 字（上限 {MAX_TEXT_CHARS}）"));
    }
    Ok(())
}

/// 参数上限（生效字号另在主题叠加之后查）。
fn validate(o: &RenderOpts) -> Result<(), String> {
    if !(o.scale.is_finite() && o.scale > 0.0 && o.scale <= MAX_SCALE) {
        return Err(format!(
            "缩放比例无效：{}（须在 (0, {MAX_SCALE}]）",
            o.scale
        ));
    }
    if !(o.font_size.is_finite() && (0.0..=MAX_FONT_SIZE).contains(&o.font_size)) {
        return Err(format!(
            "字号无效：{}（0 = 跟随主题，否则须在 (0, {MAX_FONT_SIZE}]）",
            o.font_size
        ));
    }
    if o.candidates.len() > MAX_CANDIDATES {
        return Err(format!(
            "候选过多：{} 条（上限 {MAX_CANDIDATES}）",
            o.candidates.len()
        ));
    }
    for c in &o.candidates {
        check_len("候选", &c.text)?;
        check_len("注释", &c.comment)?;
        check_len("序号标签", &c.label)?;
        check_len("上方注释", &c.comment_above)?;
    }
    if let Some(p) = &o.preedit {
        check_len("编码", p)?;
    }
    check_len("模式标签", &o.mode_label)?;
    check_len("字族名", &o.font_family)?;
    if o.page > MAX_PAGES || o.total_pages > MAX_PAGES {
        return Err(format!("页码超限（上限 {MAX_PAGES}）"));
    }
    if o.selected > MAX_CANDIDATES || o.hover > MAX_CANDIDATES as i32 {
        return Err(format!("选中 / 悬停下标超限（上限 {MAX_CANDIDATES}）"));
    }
    Ok(())
}

impl<C: Canvas> Preview<C> {
    /// 预览内置 `default` 主题、只有内置资产。
    pub fn new(canvas: C) -> Self {
        let themes = crate::builtin::theme_text(crate::builtin::DEFAULT_THEME)
            .and_then(|t| resolve_text(&t, crate::builtin::DEFAULT_THEME).ok())
            .unwrap_or_default();
        Self {
            text: CanvasText::new(canvas),
            themes,
            scene: CandidateScene::default(),
            scene_opts: CandidateSceneOptions::default(),
            assets: Arc::new(build_assets(Vec::new())),
            assets_gen: wind_scene::assets::next_generation(),
            assets_fp: assets_fingerprint(&[]),
            caches: PaintCaches::default(),
            replay_warnings: false,
            visible: false,
            anim_allowed: false,
            last: RenderOpts::default(),
        }
    }

    /// 换主题与资源（路径 → 字节，相对主题目录，如 `assets/panel.png`）。`id` 是主题自己的 id
    /// （可空；`base` 写成它视同没写，同目录加载）。解析失败返回错误原文（含 TOML 行列），当前
    /// 主题与资源不变。
    ///
    /// 资源内容没变（只改了主题文字）时沿用资源代号与图片缓存，皮肤图不重新解码；变了就换新代号、
    /// 丢掉本实例的图片缓存与动画帧。
    ///
    /// 返回解析期间引擎的 `skin:` 告警。场景随之重建，按场景去重的告警会在其后首帧再报一次；资源层
    /// （图集 / 动画建帧）的告警也在首帧由命中的缓存条目重放——只放这一帧实际用到的。
    pub fn set_theme(
        &mut self,
        toml: &str,
        id: &str,
        assets: Vec<(String, Vec<u8>)>,
    ) -> Result<Vec<String>, String> {
        let (themes, warnings) = crate::warnings::collect(|| resolve_text(toml, id));
        self.themes = themes?;
        let fp = assets_fingerprint(&assets);
        if fp != self.assets_fp {
            self.assets = Arc::new(build_assets(assets));
            self.assets_gen = wind_scene::assets::next_generation();
            self.assets_fp = fp;
            self.caches = PaintCaches::default();
        }
        self.replay_warnings = true;
        // 新场景：皮肤告警重新报、装饰状态机重建。
        self.scene = CandidateScene::default();
        self.visible = false;
        Ok(warnings)
    }

    /// 按参数搭一帧的场景输入，登记本实例的资源表、换上本实例的缓存，交给 `f`。
    fn with_input<R>(
        &mut self,
        o: &RenderOpts,
        f: impl FnOnce(&mut CandidateScene, &CandidateSceneInput) -> Result<R, String>,
    ) -> Result<R, String> {
        validate(o)?;
        self.text.set_family(&o.font_family);
        self.text.set_weight(o.font_weight);
        let src = &self.themes[usize::from(o.dark)];
        let eff = effective_theme(
            src,
            &ThemeOverrides {
                vertical: o.vertical,
                user_font_family: &o.font_family,
                user_font_weight: o.font_weight,
                scheme_text_family: "",
                shadow: o.shadow,
                font_size: o.font_size,
            },
            // 浏览器按 CSS 字族名找字体，名字原样用。
            |f| (f.to_string(), 0),
        );
        let size = eff.base_font_logical;
        if !(size.is_finite() && size > 0.0 && size <= MAX_FONT_SIZE) {
            return Err(format!(
                "生效字号 {size} 超出范围（须在 (0, {MAX_FONT_SIZE}]）"
            ));
        }
        let labels = &src.views.index_labels;
        let candidates: Vec<CandidateItem> = if o.candidates.is_empty() {
            wind_ui_types::preview_candidates(o.per_page, |i| {
                wind_ui_types::theme_index_label(labels, i)
            })
        } else {
            o.candidates
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    Ok(CandidateItem {
                        text: c.text.clone(),
                        text_styled: candidate_styled(c)?,
                        code: String::new(),
                        label: if c.label.is_empty() {
                            wind_ui_types::theme_index_label(labels, i)
                        } else {
                            c.label.clone()
                        },
                        tooltip: Default::default(),
                        comment: c.comment.clone().into(),
                        comment_above: c.comment_above.clone().into(),
                        no_index: false,
                    })
                })
                .collect::<Result<_, String>>()?
        };
        let preedit = o
            .preedit
            .as_deref()
            .unwrap_or(wind_ui_types::PREVIEW_PREEDIT);
        let caret = o
            .caret
            .filter(|&c| c <= preedit.len() && preedit.is_char_boundary(c))
            .unwrap_or(preedit.len());
        let inp = CandidateSceneInput {
            candidates: &candidates,
            preedit,
            preedit_caret: caret,
            mode_label: &o.mode_label,
            selected: o.selected,
            hover: o.hover,
            page: o.page,
            total_pages: o.total_pages,
            grid: None,
            vertical: o.vertical,
            rotated: false,
            upright: false,
            preedit_embedded: o.embedded,
            above: o.above,
            theme: &eff.theme,
            base_font_logical: eff.base_font_logical,
            scale: o.scale,
            // 当作一块 SCREEN_DP 见方的屏幕（逻辑像素）：长注释像实机一样被钳宽截断，且截断与否
            // 不随缩放变。按设备像素定死的话，放大预览时屏幕在逻辑上变窄，内容会跟着被截。
            screen_extent_px: Some({
                let px = (SCREEN_DP as f32 * o.scale).round() as u32;
                (px, px)
            }),
            text_renderer: &self.text,
            opts: &self.scene_opts,
        };
        wind_scene::assets::install(self.assets_gen, &self.assets);
        let scene = &mut self.scene;
        let r = wind_scene::view::with_paint_caches(&mut self.caches, || f(scene, &inp));
        // 宿主（JS）调用失败：这一帧作废。
        if let Some(e) = self.text.take_error() {
            return Err(e);
        }
        r
    }

    /// 本帧的动画时钟；动画开关变了让状态机重建。
    fn clock(&mut self, o: &RenderOpts, t_ms: f64) -> AnimClock {
        if o.animate != self.anim_allowed {
            self.anim_allowed = o.animate;
            self.scene.decor_anims_key = None;
        }
        AnimClock {
            t: duration_ms(t_ms),
            showing: !self.visible,
            allowed: o.animate,
        }
    }

    /// 画一帧。第一帧（或 `show` 之后）各装饰状态机从 `t_ms` 起从头播。出错（参数超限、帧超限、
    /// 宿主画字失败）时实例照常可用，下一帧照常画。
    pub fn render(&mut self, o: &RenderOpts) -> Result<Frame, String> {
        let clock = self.clock(o, o.t_ms);
        let soft_shadow = o.soft_shadow;
        let replay = self.replay_warnings;
        let (frame, warnings) = crate::warnings::collect(|| {
            let mut draw = || {
                self.with_input(o, |scene, inp| {
                    render_candidate_within(scene, inp, clock, soft_shadow, MAX_SIDE, MAX_PIXELS)
                        .map_err(|e| format!("{FRAME_TOO_LARGE}{e}"))
                })
            };
            if replay {
                wind_scene::image_cache::replaying_skin_warnings(draw)
            } else {
                draw()
            }
        });
        let frame = frame?;
        self.replay_warnings = false;
        self.visible = true;
        self.last = o.clone();
        Ok(Frame {
            width: frame.width,
            height: frame.height,
            rgba: frame.to_rgba(),
            next_ms: self.scene.next_anim_at().map(|d| d.as_secs_f64() * 1000.0),
            nodes: frame
                .nodes
                .iter()
                .map(|(k, r)| NodeRect {
                    key: k.name(),
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                })
                .collect(),
            clamped: frame.safety_clamped,
            skin: frame.skin.map(SkinInfo::from),
            warnings,
        })
    }

    /// 喂一个装饰事件（`typing` / `backspace` / `page` / `empty` / `commit` / `mode_cn` /
    /// `mode_en` / `idle_<N>s` / `click` / `click:<装饰下标>` / `show` / `hide`），时刻缺省取
    /// 上一帧的 `t_ms`。返回画面是否可能变了（变了就该再 `render` 一帧）。
    ///
    /// `show` = 下一帧当作窗口刚显示（状态机从头播）；`hide` 喂完事件后同样视为隐藏。
    pub fn signal(&mut self, name: &str, t_ms: Option<f64>) -> Result<bool, String> {
        if name == "show" {
            self.visible = false;
            return Ok(true);
        }
        let (ev, only) = parse_signal(name).ok_or_else(|| format!("未知事件：{name}"))?;
        let last = self.last.clone();
        let clock = self.clock(&last, t_ms.unwrap_or(last.t_ms));
        let changed = self.with_input(&last, |scene, inp| {
            Ok(scene.decor_event(inp, ev, only, clock))
        })?;
        if ev == DecorEvent::Hide {
            self.visible = false;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::mock::MockCanvas;

    fn preview() -> (Preview<MockCanvas>, MockCanvas) {
        let c = MockCanvas::default();
        (Preview::new(c.clone()), c)
    }

    fn opaque_px(f: &Frame) -> usize {
        f.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .count()
    }

    fn count_px(f: &Frame, rgba: [u8; 4]) -> usize {
        f.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| **p == rgba)
            .count()
    }

    /// 编码成 PNG 的纯色图。
    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        ::image::RgbaImage::from_pixel(w, h, ::image::Rgba(rgba))
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                ::image::ImageFormat::Png,
            )
            .unwrap();
        out
    }

    /// 某段文字被画时用的 CSS 字体串。
    fn font_of(c: &MockCanvas, text: &str) -> String {
        let draws = c.draws.borrow();
        draws
            .iter()
            .find(|(t, _, _)| t == text)
            .unwrap_or_else(|| panic!("没画 {text}：{draws:?}"))
            .1
            .clone()
    }

    #[test]
    fn default_theme_renders_sample_candidates() {
        let (mut p, c) = preview();
        let f = p.render(&RenderOpts::default()).unwrap();
        assert_eq!(f.rgba.len(), (f.width * f.height * 4) as usize);
        assert!(f.width > f.height, "横排是宽条：{}x{}", f.width, f.height);
        assert!(opaque_px(&f) > 0, "窗口底色应画出");
        for want in ["你好", "拟好", "wqiy", "ni'hao"] {
            font_of(&c, want);
        }
        assert!(
            !c.draws.borrow().iter().any(|(t, _, _)| t == "匿"),
            "出厂 7 条，不画第 10 条"
        );
        assert_eq!(f.next_ms, None, "无皮肤无动画");
    }

    #[test]
    fn candidates_take_comment_above_and_pinyin_hint() {
        let (mut p, c) = preview();
        let o: RenderOpts = serde_json::from_str(
            r#"{"candidates":[{"text":"中(zhong)文","hint":[3,10],"comment_above":"zhōng wén"},{"text":"众"}]}"#,
        )
        .unwrap();
        let f = p.render(&o).unwrap();
        font_of(&c, "zhōng wén");
        assert!(f.nodes.iter().any(|n| n.key == "comment_above"));
        let bad: RenderOpts =
            serde_json::from_str(r#"{"candidates":[{"text":"中文","hint":[1,3]}]}"#).unwrap();
        let e = p.render(&bad).err().unwrap();
        assert!(e.contains("字符边界"), "{e}");
    }

    #[test]
    fn frame_carries_node_rects_in_device_pixels() {
        let (mut p, _) = preview();
        let one = p.render(&RenderOpts::default()).unwrap();
        let two = p
            .render(&RenderOpts {
                scale: 2.0,
                ..Default::default()
            })
            .unwrap();
        let keys: Vec<&str> = one.nodes.iter().map(|n| n.key).collect();
        for want in [
            "window",
            "preedit_bar",
            "candidate_list",
            "item",
            "index",
            "footer_bar",
        ] {
            assert!(keys.contains(&want), "缺 {want}：{keys:?}");
        }
        assert_eq!(keys.iter().filter(|k| **k == "item").count(), 7);
        let item = |f: &Frame| f.nodes.iter().find(|n| n.key == "item").unwrap().clone();
        let (a, b) = (item(&one), item(&two));
        assert!(
            (b.w - a.w * 2.0).abs() <= 2.0,
            "设备像素随 scale：{a:?} / {b:?}"
        );
        for n in &two.nodes {
            assert!(n.x >= 0.0 && n.y >= 0.0, "{n:?}");
            assert!(n.x + n.w <= two.width as f32 + 0.5, "{n:?}");
            assert!(n.y + n.h <= two.height as f32 + 0.5, "{n:?}");
        }
        let json = serde_json::to_string(&one.nodes[0]).unwrap();
        assert!(json.starts_with(r#"{"key":"window","x":"#), "{json}");
    }

    #[test]
    fn opts_parse_from_partial_json_and_change_layout() {
        let (mut p, c) = preview();
        let o: RenderOpts = serde_json::from_str(
            r#"{"vertical":true,"scale":2,"font_size":15,"candidates":[{"text":"甲"},{"text":"乙","comment":"注","label":"b"}],"preedit":"jy"}"#,
        )
        .unwrap();
        let v = p.render(&o).unwrap();
        assert!(v.height > v.width, "竖排是高条：{}x{}", v.width, v.height);
        for want in ["乙", "注", "b", "jy"] {
            font_of(&c, want);
        }
        // 候选正文按生效字号 × 缩放画。
        assert!(
            font_of(&c, "甲").contains(" 30px "),
            "{}",
            font_of(&c, "甲")
        );
    }

    #[test]
    fn dark_and_shadow_change_pixels() {
        let (mut p, _) = preview();
        let light = p.render(&RenderOpts::default()).unwrap();
        let dark = p
            .render(&RenderOpts {
                dark: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!((light.width, light.height), (dark.width, dark.height));
        assert_ne!(light.rgba, dark.rgba, "明暗换色");
        let bare = p
            .render(&RenderOpts {
                soft_shadow: false,
                ..Default::default()
            })
            .unwrap();
        assert!(bare.width < light.width && bare.height < light.height);
    }

    #[test]
    fn user_font_family_reaches_css_font() {
        let (mut p, c) = preview();
        p.render(&RenderOpts {
            font_family: "Noto Serif SC".into(),
            font_weight: 700,
            ..Default::default()
        })
        .unwrap();
        let f = font_of(&c, "你好");
        assert!(f.contains("\"Noto Serif SC\""), "{f}");
        assert!(f.starts_with("700 "), "{f}");
    }

    #[test]
    fn set_theme_errors_verbatim_and_keeps_current_theme() {
        let (mut p, _) = preview();
        let before = p.render(&RenderOpts::default()).unwrap().rgba;
        let e = p.set_theme("[meta\nname = 1", "", Vec::new()).unwrap_err();
        assert!(e.contains("parse theme"), "{e}");
        let e = p
            .set_theme("base = \"nope\"\n", "", Vec::new())
            .unwrap_err();
        assert!(e.contains("\"nope\"") && e.contains("只认内置主题"), "{e}");
        assert_eq!(p.render(&RenderOpts::default()).unwrap().rgba, before);
        // 编辑的就是 default 本身：base 指向自己视同没写。
        p.set_theme(
            "base = \"mine\"\n[meta]\nname = \"m\"\n",
            "mine",
            Vec::new(),
        )
        .unwrap();
    }

    /// 参数上限：每一条都报错、不 panic、不卡死，且实例之后照常可用。
    #[test]
    fn oversized_params_are_rejected() {
        let (mut p, _) = preview();
        let d = RenderOpts::default;
        let many = vec![
            CandidateOpt {
                text: "字".into(),
                ..Default::default()
            };
            MAX_CANDIDATES + 1
        ];
        let long = CandidateOpt {
            text: "字".repeat(MAX_TEXT_CHARS + 1),
            ..Default::default()
        };
        let cases = [
            RenderOpts { scale: 0.0, ..d() },
            RenderOpts { scale: 9.0, ..d() },
            RenderOpts {
                scale: f32::NAN,
                ..d()
            },
            RenderOpts {
                font_size: 100_000.0,
                ..d()
            },
            RenderOpts {
                font_size: f32::INFINITY,
                ..d()
            },
            RenderOpts {
                font_size: -1.0,
                ..d()
            },
            RenderOpts {
                candidates: many,
                ..d()
            },
            RenderOpts {
                candidates: vec![long],
                ..d()
            },
            RenderOpts {
                preedit: Some("a".repeat(MAX_TEXT_CHARS + 1)),
                ..d()
            },
            RenderOpts {
                total_pages: MAX_PAGES + 1,
                ..d()
            },
            RenderOpts {
                selected: 1 << 40,
                ..d()
            },
        ];
        for (i, o) in cases.iter().enumerate() {
            assert!(p.render(o).is_err(), "第 {i} 条应报错");
        }
        // 字号在上限内、缩放顶格：帧超限，在分配缓冲之前报错。
        let e = p
            .render(&RenderOpts {
                font_size: MAX_FONT_SIZE,
                scale: MAX_SCALE,
                ..d()
            })
            .err()
            .expect("帧超限");
        assert!(e.contains("超出上限"), "{e}");
        // 时刻钳住而不是 panic。
        for t in [1e300, f64::INFINITY, f64::NAN, -5.0] {
            p.render(&RenderOpts {
                t_ms: t,
                animate: true,
                ..d()
            })
            .unwrap();
        }
        assert!(p.render(&d()).is_ok(), "报错之后照常可用");
    }

    /// 长注释在高缩放下像实机一样被钳宽，能出图，不报「超出上限」。
    #[test]
    fn long_comment_is_clamped_in_width_not_rejected() {
        let (mut p, _) = preview();
        let f = p
            .render(&RenderOpts {
                scale: 2.0,
                candidates: vec![CandidateOpt {
                    text: "长".into(),
                    comment: "注".repeat(MAX_TEXT_CHARS),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
        assert!(f.width <= MAX_SIDE, "{}", f.width);
        assert!(f.clamped, "过了预览屏宽");
    }

    /// 预览屏按逻辑像素定：换缩放不改截断（宽度随缩放等比），放大到撞尺寸闸报 FRAME_TOO_LARGE。
    #[test]
    fn screen_is_logical_so_zoom_keeps_content() {
        let (mut p, _) = preview();
        let o = |scale: f32| RenderOpts {
            scale,
            candidates: vec![CandidateOpt {
                text: "长".into(),
                comment: "注".repeat(150),
                ..Default::default()
            }],
            ..Default::default()
        };
        let one = p.render(&o(1.0)).unwrap();
        let two = p.render(&o(2.0)).unwrap();
        assert!(!one.clamped && !two.clamped);
        // 等比（差的是各处取整）。
        assert!(
            (two.width as f32 - one.width as f32 * 2.0).abs() <= one.width as f32 * 0.02,
            "{} vs {}",
            one.width,
            two.width
        );
        let e = p.render(&o(4.0)).err().expect("4 倍撞 4096");
        assert!(e.starts_with(FRAME_TOO_LARGE), "{e}");
        assert!(p.render(&o(1.0)).is_ok());
    }

    /// 主题里的字号走同一道闸。
    #[test]
    fn theme_font_size_is_bounded_too() {
        let (mut p, _) = preview();
        p.set_theme(
            "base = \"default\"\n[behavior]\nfont_size = 100000\n",
            "",
            Vec::new(),
        )
        .unwrap();
        let e = p
            .render(&RenderOpts::default())
            .err()
            .expect("主题字号超限");
        assert!(e.contains("生效字号"), "{e}");
        assert!(
            p.render(&RenderOpts {
                font_size: 16.0,
                ..Default::default()
            })
            .is_ok(),
            "用户字号覆盖后可画"
        );
    }

    /// 宿主（JS）调用失败：这一帧报错，实例不报废，恢复后照常画。
    #[test]
    fn host_failure_fails_the_frame_only() {
        let (mut p, c) = preview();
        let good = p.render(&RenderOpts::default()).unwrap().rgba;
        c.fail.set(true);
        let e = p
            .render(&RenderOpts {
                font_family: "X".into(), // 换字族清测量缓存，迫使再问宿主
                ..Default::default()
            })
            .err()
            .expect("宿主失败");
        assert!(e.contains("mock"), "{e}");
        c.fail.set(false);
        assert_eq!(p.render(&RenderOpts::default()).unwrap().rgba, good);
    }

    /// 继承内置主题、背景图来自调用方的资源表：底图的颜色出现在帧里。
    #[test]
    fn theme_with_base_and_memory_asset() {
        let (mut p, _) = preview();
        p.set_theme(
            &bg_theme("bg.png"),
            "",
            vec![("bg.png".into(), png(4, 4, [0, 200, 0, 255]))],
        )
        .unwrap();
        let f = p.render(&RenderOpts::default()).unwrap();
        assert!(count_px(&f, [0, 200, 0, 255]) > 100, "底图应铺开");
    }

    fn bg_theme(r: &str) -> String {
        format!(
            "base = \"default\"\n[meta]\nname = \"t\"\n\
             [window]\nbackground = {{ image = {{ ref = \"{r}\", mode = \"stretch\" }} }}\n"
        )
    }

    /// 同线程两个实例：各持资源表与缓存，一方换主题 / 资源不让另一方的图失效。
    #[test]
    fn two_previews_do_not_clobber_each_other() {
        let (mut a, _) = preview();
        let (mut b, _) = preview();
        let green = [0, 200, 0, 255];
        let blue = [0, 0, 200, 255];
        a.set_theme(
            &bg_theme("bg.png"),
            "",
            vec![("bg.png".into(), png(4, 4, green))],
        )
        .unwrap();
        let fa = a.render(&RenderOpts::default()).unwrap();
        b.set_theme(
            &bg_theme("bg.png"),
            "",
            vec![("bg.png".into(), png(4, 4, blue))],
        )
        .unwrap();
        let fb = b.render(&RenderOpts::default()).unwrap();
        assert!(count_px(&fb, blue) > 100);
        assert_eq!(
            a.render(&RenderOpts::default()).unwrap().rgba,
            fa.rgba,
            "A 不受 B 影响"
        );
        b.set_theme(&bg_theme("bg.png"), "", Vec::new()).unwrap();
        assert_eq!(
            a.render(&RenderOpts::default()).unwrap().rgba,
            fa.rgba,
            "B 换资源也不影响 A"
        );
    }

    /// 资源没变时沿用代号与缓存（只改主题文字不重新解码）；变了换代号。
    #[test]
    fn unchanged_assets_keep_generation() {
        let (mut p, _) = preview();
        let assets = || vec![("bg.png".into(), png(4, 4, [0, 200, 0, 255]))];
        p.set_theme(&bg_theme("bg.png"), "", assets()).unwrap();
        let g = p.assets_gen;
        p.set_theme(
            &format!("{}\n[behavior]\nfont_size = 18\n", bg_theme("bg.png")),
            "",
            assets(),
        )
        .unwrap();
        assert_eq!(p.assets_gen, g, "资源没变沿用代号");
        p.set_theme(
            &bg_theme("bg.png"),
            "",
            vec![("bg.png".into(), png(4, 4, [1, 2, 3, 255]))],
        )
        .unwrap();
        assert_ne!(p.assets_gen, g, "内容变了换代号");
    }

    /// 两帧 GIF 当装饰：`idle` 停第 0 帧（红）、`typing` 转 `busy`（第 1 帧，蓝）。
    fn skin_theme(anim: &str) -> (String, Vec<(String, Vec<u8>)>) {
        let toml = format!(
            "[meta]\nname = \"s\"\n[skin]\ndensity = 1\n\
             [skin.horizontal.frame]\nimage = \"frame.png\"\n\
             stretch_x = [[4, 12]]\nstretch_y = [[4, 12]]\n\
             [[skin.horizontal.decor]]\nimage = \"dot.gif\"\nanchor = \"frame.top_left\"\n{anim}\n"
        );
        let assets = vec![
            ("frame.png".into(), png(16, 16, [200, 200, 200, 255])),
            ("dot.gif".into(), gif2([255, 0, 0, 255], [0, 0, 255, 255])),
        ];
        (toml, assets)
    }

    fn at(p: &mut Preview<MockCanvas>, t: f64) -> Frame {
        p.render(&RenderOpts {
            animate: true,
            t_ms: t,
            ..Default::default()
        })
        .unwrap()
    }

    /// 皮肤动画：装饰按 `t_ms` 换帧，`next_ms` 报下一次换帧；不播动画停在首帧。
    #[test]
    fn skin_animation_follows_t_ms() {
        let (mut p, _) = preview();
        let (toml, assets) = skin_theme("");
        p.set_theme(&toml, "", assets).unwrap();
        let f0 = at(&mut p, 0.0);
        assert_eq!(f0.next_ms, Some(100.0), "两帧各 100ms");
        let f1 = at(&mut p, 150.0);
        assert_ne!(f0.rgba, f1.rgba, "第二帧换了颜色");
        let f2 = at(&mut p, 250.0);
        assert_eq!(f0.rgba, f2.rgba, "循环回第一帧");
        let still = p.render(&RenderOpts::default()).unwrap();
        assert_eq!(still.next_ms, None);
    }

    /// `signal` 真的驱动状态机：`typing` 之后装饰换到 `busy` 的帧。
    #[test]
    fn signal_drives_decor_state_machine() {
        let (mut p, _) = preview();
        let (toml, assets) = skin_theme(
            "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
        );
        p.set_theme(&toml, "", assets).unwrap();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let f0 = at(&mut p, 0.0);
        assert!(
            count_px(&f0, red) > 0 && count_px(&f0, blue) == 0,
            "初始 idle = 红"
        );
        assert_eq!(at(&mut p, 500.0).rgba, f0.rgba, "没事件就停在 idle");
        assert!(p.signal("typing", Some(600.0)).unwrap(), "typing 改了画面");
        let f1 = at(&mut p, 650.0);
        assert!(
            count_px(&f1, blue) > 0 && count_px(&f1, red) == 0,
            "typing 后 = 蓝"
        );
        // show 只让常驻状态从头播，不回 idle；换主题才回初始状态。
        assert!(p.signal("show", None).unwrap());
        assert_eq!(at(&mut p, 700.0).rgba, f1.rgba);
        let (toml, assets) = skin_theme(
            "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
        );
        p.set_theme(&toml, "", assets).unwrap();
        assert_eq!(at(&mut p, 800.0).rgba, f0.rgba, "换主题回到 idle");
        assert!(p.signal("click:0", Some(10.0)).is_ok());
        assert!(p.signal("nonsense", None).is_err());
        assert!(p.signal("click:x", None).is_err());
    }

    /// 皮肤回执夹具：主坐标 density 2，窗体 32×32 两轴各一条带（y 轴 tile），四边内缩 6，
    /// 一块伸出窗体右上角的装饰（z 120、block）；`free` 时再分槽位（候选锚内容框左上、翻页锚窗体右下）。
    fn geom_theme(free: bool, extra: &str) -> (String, Vec<(String, Vec<u8>)>) {
        let slots = if free {
            "[skin.horizontal.slots.candidates]\nanchor = \"content.top_left\"\n\
             [skin.horizontal.slots.footer]\nanchor = \"frame.bottom_right\"\noffset = [-8, -4]\n"
        } else {
            ""
        };
        let toml = format!(
            "[meta]\nname = \"g\"\n[skin]\ndensity = 2\n\
             [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
             stretch_x = [[8, 24]]\nstretch_y = [{{ from = 8, to = 24, fill = \"tile\" }}]\n\
             [skin.horizontal.content]\nleft = 6\ntop = 6\nright = 6\nbottom = 6\n\
             [skin.horizontal.placement]\ncaret = {{ anchor = \"content.top_left\", offset = [2, 4] }}\n\
             {slots}{extra}\
             [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nanchor = \"frame.top_right\"\n\
             pivot = \"center\"\noffset = [-4, 0]\nz = 120\nhit = \"block\"\n"
        );
        let assets = vec![
            ("assets/frame.png".into(), png(32, 32, [200, 200, 200, 255])),
            ("assets/dot.png".into(), png(12, 12, [255, 0, 0, 255])),
        ];
        (toml, assets)
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RectInfo {
        RectInfo { x, y, w, h }
    }

    fn rect_i((x, y, w, h): (i32, i32, u32, u32)) -> RectInfo {
        rect(x as f32, y as f32, w as f32, h as f32)
    }

    /// 回执的皮肤几何就是引擎这一帧求出的那份（stack / free × 1x / 2x），与 `nodes` 同坐标。
    #[test]
    fn skin_receipt_matches_engine_geometry() {
        for free in [false, true] {
            let (mut p, _) = preview();
            let (toml, assets) = geom_theme(free, "");
            let w = p.set_theme(&toml, "", assets).unwrap();
            assert!(w.is_empty(), "{w:?}");
            for scale in [1.0f32, 2.0] {
                let f = p
                    .render(&RenderOpts {
                        scale,
                        ..Default::default()
                    })
                    .unwrap();
                let s = f.skin.clone().expect("启用了皮肤");
                let g = p.scene.skin_geom.as_ref().unwrap();
                let k = scale / 2.0;
                let ctx = format!("free={free} scale={scale}");
                assert_eq!(s.variant, "horizontal", "{ctx}");
                assert_eq!((s.k, s.render_scale), (k, scale), "{ctx}");
                assert_eq!(s.frame_source, "assets/frame.png");
                assert_eq!(s.min_size, [g.min_w, g.min_h], "{ctx}");
                let fr = rect_i(g.frame_for_test());
                assert_eq!(s.frame, fr, "{ctx}");
                let window = f.nodes.iter().find(|n| n.key == "window").unwrap();
                // 窗体是根容器盒向上取整。
                assert!(
                    (window.x, window.y) == (fr.x, fr.y)
                        && (0.0..1.0).contains(&(fr.w - window.w))
                        && (0.0..1.0).contains(&(fr.h - window.h)),
                    "窗体即根容器盒，与 nodes 同坐标（{ctx}）：{window:?} / {fr:?}"
                );
                let i = 6.0 * k;
                assert_eq!(
                    s.content,
                    rect(fr.x + i, fr.y + i, fr.w - 2.0 * i, fr.h - 2.0 * i),
                    "{ctx}"
                );
                let rects: Vec<RectInfo> = g
                    .decor_rects_for_test()
                    .iter()
                    .map(|&r| rect_i(r))
                    .collect();
                assert_eq!(
                    s.decor.iter().map(|d| d.rect).collect::<Vec<_>>(),
                    rects,
                    "{ctx}"
                );
                let d = &s.decor[0];
                assert!(
                    d.rect.y < fr.y && d.rect.x + d.rect.w > fr.x + fr.w,
                    "外伸：{d:?}"
                );
                assert_eq!((d.index, d.z, d.hit), (0, 120, "block"));
                assert_eq!(
                    d.clip,
                    ClipInfo {
                        state: None,
                        frame: 0,
                        raw: Some(0)
                    }
                );
                // 切片（Pair 带缺省 round）：源像素按 frame.image 写，目标覆盖整个窗体。
                for (axis, origin, len, tiled) in [
                    (&s.axis_x, fr.x, fr.w, "round"),
                    (&s.axis_y, fr.y, fr.h, "tile"),
                ] {
                    let kinds: Vec<&str> = axis.iter().map(|a| a.kind).collect();
                    assert_eq!(kinds, ["fixed", tiled, "fixed"], "{ctx}");
                    assert_eq!(axis[1].src, [8.0, 24.0], "{ctx}");
                    assert_eq!(axis[0].dst[0], origin);
                    assert_eq!(axis[2].dst[1], origin + len);
                    assert_eq!(axis[0].dst[1] - axis[0].dst[0], 8.0 * scale / 2.0);
                }
                assert_eq!(
                    s.caret.below,
                    [fr.x + i + 2.0 * k, fr.y + i + 4.0 * k],
                    "{ctx}"
                );
                assert_eq!(s.caret.above, [fr.x, fr.y + fr.h], "缺省对齐窗体左下");
                if free {
                    let names: Vec<&str> = s.slots.iter().map(|x| x.name).collect();
                    assert_eq!(names, ["candidates", "footer"], "{ctx}");
                    assert_eq!(
                        s.slots.iter().map(|x| x.on).collect::<Vec<_>>(),
                        ["content", "frame"]
                    );
                    let sizes = g.parts_for_test();
                    let pos = g.part_positions_for_test(fr.w, fr.h);
                    for ((slot, (_, w, h)), (x, y)) in s.slots.iter().zip(sizes).zip(pos) {
                        assert_eq!(slot.rect, rect(fr.x + x, fr.y + y, w, h), "{ctx}");
                    }
                    let footer = s.slots[1].rect;
                    assert_eq!(
                        (footer.x + footer.w, footer.y + footer.h),
                        (fr.x + fr.w - 8.0 * k, fr.y + fr.h - 4.0 * k),
                        "{ctx}"
                    );
                } else {
                    assert!(s.slots.is_empty());
                }
                let json = serde_json::to_string(&s).unwrap();
                for key in ["renderScale", "frameSource", "minSize", "axisX", "axisY"] {
                    assert!(json.contains(&format!("\"{key}\":")), "{key}：{json}");
                }
                assert_eq!(json.contains("\"box\":"), free, "{json}");
            }
        }
    }

    /// 回执报实际所用的变体：嵌入没配回退非嵌入；配了用嵌入；竖排没配不启用皮肤。
    #[test]
    fn skin_receipt_reports_variant_after_fallback() {
        let (mut p, _) = preview();
        let (toml, assets) = geom_theme(false, "");
        p.set_theme(&toml, "", assets.clone()).unwrap();
        let embedded = RenderOpts {
            embedded: true,
            ..Default::default()
        };
        assert_eq!(
            p.render(&embedded).unwrap().skin.unwrap().variant,
            "horizontal"
        );
        let vertical = RenderOpts {
            vertical: true,
            ..Default::default()
        };
        assert!(
            p.render(&vertical).unwrap().skin.is_none(),
            "竖排不回退横排"
        );
        let own = format!("{toml}[skin.horizontal_embedded.frame]\nimage = \"assets/frame.png\"\n");
        p.set_theme(&own, "", assets).unwrap();
        assert_eq!(
            p.render(&embedded).unwrap().skin.unwrap().variant,
            "horizontal_embedded"
        );
        assert!(p.render(&RenderOpts::default()).unwrap().skin.is_some());
        p.set_theme(&bg_theme("bg.png"), "", Vec::new()).unwrap();
        assert!(
            p.render(&RenderOpts::default()).unwrap().skin.is_none(),
            "无皮肤"
        );
    }

    /// 引擎的 `skin:` 告警原文随回执带出：解析期的随 `set_theme`，出图期的随帧；非 `skin:` 的不进来。
    #[test]
    fn skin_warnings_come_back_verbatim() {
        let (mut p, _) = preview();
        // 窗体图密度 0.001：倍率 1000，32 px 的图要缩放成 32000 px，引擎整套回退普通窗口。
        // 窗口底图 9000×1 超出位图上限：引擎也告警，但不是 `skin:` 前缀。
        let toml = "[meta]\nname = \"w\"\n\
                    [window]\nbackground = { image = { ref = \"assets/wide.png\", mode = \"stretch\" } }\n\
                    [skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/frame.png\"\ndensity = 0.001\n\
                    stretch_x = [{ from = 8, to = 24, fill = \"zigzag\" }]\n";
        let assets = vec![
            ("assets/frame.png".into(), png(32, 32, [200, 200, 200, 255])),
            ("assets/wide.png".into(), png(9000, 1, [0, 0, 0, 255])),
        ];
        let w = p.set_theme(toml, "", assets).unwrap();
        assert!(
            w.len() == 1 && w[0].starts_with("skin: 未知拉伸带填法"),
            "解析期：{w:?}"
        );
        let f = p.render(&RenderOpts::default()).unwrap();
        assert!(f.skin.is_none(), "回退普通窗口");
        assert!(
            f.warnings.len() == 1
                && f.warnings[0].starts_with("skin: 窗体图按倍率 ")
                && f.warnings[0].ends_with(" 缩放后超出上限，回退普通窗口"),
            "只收 skin: 前缀：{:?}",
            f.warnings
        );
        // 同一场景按场景去重，下一帧不再报；换主题（新场景）后首帧再报。
        assert!(
            p.render(&RenderOpts::default())
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    /// 装饰的片段与帧号：`idle` 第 0 帧；`typing` 后 `busy`（原始第 1 帧）。
    #[test]
    fn skin_receipt_reports_decor_clip() {
        let (mut p, _) = preview();
        let (toml, assets) = skin_theme(
            "states = { idle = { frames = \"0\" }, busy = { frames = \"1\" } }\n\
             on = { typing = { state = \"busy\" } }",
        );
        p.set_theme(&toml, "", assets).unwrap();
        let clip = |f: Frame| f.skin.unwrap().decor[0].clip.clone();
        assert_eq!(
            clip(at(&mut p, 0.0)),
            ClipInfo {
                state: Some("idle".into()),
                frame: 0,
                raw: Some(0)
            }
        );
        p.signal("typing", Some(10.0)).unwrap();
        assert_eq!(
            clip(at(&mut p, 20.0)),
            ClipInfo {
                state: Some("busy".into()),
                frame: 0,
                raw: Some(1)
            }
        );
    }

    /// 帧多、目标尺寸大的动图装饰：12 帧 × 800² 超 24 MiB 帧预算，引擎隔帧丢弃（stride 2）。
    /// `idle` 取原始 4–5 帧。
    fn dropping_theme(extra: &str) -> (String, Vec<(String, Vec<u8>)>) {
        let toml = format!(
            "[meta]\nname = \"d\"\n{extra}[skin]\ndensity = 1\n\
             [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
             [[skin.horizontal.decor]]\nimage = \"assets/anim.gif\"\nsize = [800, 800]\n\
             states = {{ idle = {{ frames = \"4-5\" }} }}\n"
        );
        let frames: Vec<([u8; 4], u32)> = (0..12).map(|i| ([i * 20, 0, 0, 255], 100)).collect();
        let assets = vec![
            ("assets/frame.png".into(), png(16, 16, [200, 200, 200, 255])),
            (
                "assets/anim.gif".into(),
                wind_scene::image_cache::anim_fixtures::gif(4, 4, &frames),
            ),
        ];
        (toml, assets)
    }

    fn is_drop(w: &str) -> bool {
        w.starts_with("skin: 动画帧超出 ") && w.contains("隔帧丢弃到 6 帧")
    }

    /// 资源层告警（降帧）只在建缓存时报：资源没变、只改 TOML 再 `set_theme`，首帧由命中的缓存重放；
    /// 改小装饰尺寸（换了缓存键、不再降帧）后不再报。降帧后 `raw` = 保留帧 × 步长。
    #[test]
    fn resource_warnings_survive_toml_only_set_theme() {
        let (mut p, _) = preview();
        let (toml, assets) = dropping_theme("");
        assert!(p.set_theme(&toml, "", assets.clone()).unwrap().is_empty());
        let f = at(&mut p, 0.0);
        assert!(f.warnings.iter().any(|w| is_drop(w)), "{:?}", f.warnings);
        let clip = &f.skin.as_ref().unwrap().decor[0].clip;
        assert_eq!(
            (clip.state.as_deref(), clip.raw),
            (Some("idle"), Some(4)),
            "原始 4–5 帧隔帧丢弃后落在保留的第 2 帧：2 × 2 = 4"
        );
        assert!(at(&mut p, 10.0).warnings.is_empty(), "同场景不重复报");
        // 只改 TOML、资源不变：缓存沿用、引擎不会再报，首帧由命中的缓存条目重放。
        let (toml2, _) = dropping_theme("[behavior]\nfont_size = 18\n");
        assert!(p.set_theme(&toml2, "", assets.clone()).unwrap().is_empty());
        let f = at(&mut p, 20.0);
        assert!(
            f.warnings.iter().any(|w| is_drop(w)),
            "首帧重放：{:?}",
            f.warnings
        );
        assert!(at(&mut p, 30.0).warnings.is_empty(), "其后不重复");
        // 改小尺寸：12 帧 × 200² 在预算内，不再降帧；旧条目没被这一帧用到，不重放。
        let smaller = toml2.replace("size = [800, 800]", "size = [200, 200]");
        assert!(
            p.set_theme(&smaller, "", assets.clone())
                .unwrap()
                .is_empty()
        );
        let f = at(&mut p, 40.0);
        assert!(!f.warnings.iter().any(|w| is_drop(w)), "{:?}", f.warnings);
        assert_eq!(
            f.skin.unwrap().decor[0].clip.raw,
            Some(4),
            "不降帧：原始第 4 帧"
        );
        // 改回大尺寸：旧条目又被用到，重放。
        p.set_theme(&toml2, "", assets).unwrap();
        assert!(at(&mut p, 50.0).warnings.iter().any(|w| is_drop(w)));
    }

    /// `index` 是求值后（z 排序、丢掉无效项）的下标，`src_index` 是 TOML 里的原始下标。
    #[test]
    fn decor_receipt_carries_toml_index() {
        let (mut p, _) = preview();
        let toml = "[meta]\nname = \"z\"\n[skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/frame.png\"\n\
                    [[skin.horizontal.decor]]\nimage = \"\"\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nz = 5\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/dot.png\"\nz = -3\n\
                    [[skin.horizontal.decor]]\nimage = \"assets/missing.png\"\n";
        let assets = vec![
            ("assets/frame.png".into(), png(16, 16, [200, 200, 200, 255])),
            ("assets/dot.png".into(), png(4, 4, [255, 0, 0, 255])),
        ];
        p.set_theme(toml, "", assets).unwrap();
        let s = p.render(&RenderOpts::default()).unwrap().skin.unwrap();
        let got: Vec<(usize, usize, i32)> = s
            .decor
            .iter()
            .map(|d| (d.index, d.src_index, d.z))
            .collect();
        // 求值后：[0] z -3（TOML 2）、[1] 缺图（TOML 3，渲染时丢）、[2] z 5（TOML 1）；TOML 0 空图解析时丢。
        assert_eq!(got, [(0, 2, -3), (2, 1, 5)]);
        let json = serde_json::to_string(&s.decor[0]).unwrap();
        assert!(json.contains("\"srcIndex\":2"), "{json}");
    }

    /// 选到 srcset 里的另一份源图：`frameSource` 报它，`axis*.src` 仍按 `frame.image` 的像素。
    #[test]
    fn srcset_pick_keeps_axis_in_main_image_pixels() {
        let (mut p, _) = preview();
        let toml = "[meta]\nname = \"x\"\n[skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"assets/f1.png\"\nstretch_x = [[8, 24]]\n\
                    srcset = [{ image = \"assets/f2.png\", density = 2 }]\n";
        let assets = vec![
            ("assets/f1.png".into(), png(32, 32, [200, 200, 200, 255])),
            ("assets/f2.png".into(), png(64, 64, [100, 100, 100, 255])),
        ];
        p.set_theme(toml, "", assets).unwrap();
        let at_scale = |p: &mut Preview<MockCanvas>, scale| {
            p.render(&RenderOpts {
                scale,
                ..Default::default()
            })
            .unwrap()
            .skin
            .unwrap()
        };
        let one = at_scale(&mut p, 1.0);
        assert_eq!(one.frame_source, "assets/f1.png");
        let two = at_scale(&mut p, 2.0);
        assert_eq!(two.frame_source, "assets/f2.png");
        for s in [&one, &two] {
            assert_eq!(s.axis_x[1].src, [8.0, 24.0], "{}", s.frame_source);
            assert_eq!(s.axis_x[2].src, [24.0, 32.0]);
            let fixed = s.axis_x[0].dst[1] - s.axis_x[0].dst[0];
            assert_eq!(fixed, 8.0 * s.render_scale, "{}", s.frame_source);
        }
    }

    /// 竖排变体与上方布局：回执几何同样就是引擎这一帧的那份。
    #[test]
    fn vertical_and_above_receipts_match_engine_geometry() {
        let (mut p, _) = preview();
        let (toml, assets) = geom_theme(false, "");
        let toml = toml.replace("skin.horizontal", "skin.vertical");
        p.set_theme(&toml, "", assets.clone()).unwrap();
        let o = RenderOpts {
            vertical: true,
            scale: 2.0,
            ..Default::default()
        };
        let f = p.render(&o).unwrap();
        let s = f.skin.unwrap();
        let g = p.scene.skin_geom.as_ref().unwrap();
        assert_eq!(s.variant, "vertical");
        assert!(f.height > f.width, "{}x{}", f.width, f.height);
        assert_eq!(s.frame, rect_i(g.frame_for_test()));
        let rects: Vec<RectInfo> = g
            .decor_rects_for_test()
            .iter()
            .map(|&r| rect_i(r))
            .collect();
        assert_eq!(s.decor.iter().map(|d| d.rect).collect::<Vec<_>>(), rects);
        assert!(
            p.render(&RenderOpts::default()).unwrap().skin.is_none(),
            "横排没配"
        );

        let (toml, _) = geom_theme(false, "");
        p.set_theme(&toml, "", assets).unwrap();
        let above = p
            .render(&RenderOpts {
                above: true,
                ..Default::default()
            })
            .unwrap();
        let s = above.skin.unwrap();
        let g = p.scene.skin_geom.as_ref().unwrap();
        let fr = rect_i(g.frame_for_test());
        assert_eq!(s.frame, fr);
        let window = above.nodes.iter().find(|n| n.key == "window").unwrap();
        assert_eq!((window.x, window.y), (fr.x, fr.y), "上方重排不挪窗体");
        assert_eq!(s.caret.above, [fr.x, fr.y + fr.h]);
    }

    /// 两帧 GIF（各 100ms），4×4 纯色。
    fn gif2(a: [u8; 4], b: [u8; 4]) -> Vec<u8> {
        use ::image::codecs::gif::{GifEncoder, Repeat};
        use ::image::{Delay, Frame as ImgFrame, RgbaImage};
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).unwrap();
            for c in [a, b] {
                let img = RgbaImage::from_pixel(4, 4, ::image::Rgba(c));
                enc.encode_frame(ImgFrame::from_parts(
                    img,
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
            }
        }
        out
    }
}
