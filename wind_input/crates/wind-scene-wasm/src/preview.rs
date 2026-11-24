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
    /// 镜像：`x` / `y` / `xy`（同主题写法）；null = 不翻。只翻像素，`rect` 按源图算、与不翻时相同。
    pub flip: Option<&'static str>,
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
                    flip: d.flip.name(),
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
#[path = "preview_tests.rs"]
mod tests;
