//! 预览 API 本体（与 JS 无关，原生目标上用 mock 画布单测）：主题文本 + 资源 → 一帧候选窗 RGBA。
//!
//! 渲染走与设置端预览（wind-ui `candidate_window/preview.rs`）同一条路：
//! [`effective_theme`] 叠外观覆盖 → [`render_candidate`]。动画由调用方给时刻（`t_ms`），
//! 事件经 [`Preview::signal`] 喂装饰状态机。

use std::time::Duration;

use serde::Deserialize;
use wind_scene::candidate::{
    AnimClock, CandidateScene, CandidateSceneInput, CandidateSceneOptions, ThemeOverrides,
    effective_theme, render_candidate,
};
use wind_theme::Resolved;
use wind_theme::skin::DecorEvent;
use wind_ui_types::CandidateItem;

use crate::canvas::{Canvas, CanvasText};

/// 一条自定义候选。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CandidateOpt {
    pub text: String,
    pub comment: String,
    /// 序号标签；空 = 主题槽位，再空按数字。
    pub label: String,
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
}

pub struct Preview<C> {
    text: CanvasText<C>,
    /// 当前主题的 `[亮, 暗]` 求值结果。
    themes: [Resolved; 2],
    scene: CandidateScene,
    scene_opts: CandidateSceneOptions,
    /// 上一帧已画出（装饰状态机视窗口为显示中）。
    visible: bool,
    /// 状态机按哪种动画开关建的。
    anim_allowed: bool,
    /// 上一帧的参数（`signal` 用它搭场景输入）。
    last: RenderOpts,
}

/// `t_ms` → 时间轴时长（负数、NaN 当 0）。
fn duration_ms(t_ms: f64) -> Duration {
    if t_ms.is_finite() && t_ms > 0.0 {
        Duration::from_secs_f64(t_ms / 1000.0)
    } else {
        Duration::ZERO
    }
}

/// 主题文本 → `[亮, 暗]`，base 链在内置主题里找。
fn resolve_text(toml: &str) -> Result<[Resolved; 2], String> {
    let t = wind_theme::load_typed_text(toml, &crate::builtin::theme_text)
        .map_err(|e| format!("{e:#}"))?;
    Ok([
        wind_theme::resolve(&t, false, &[]),
        wind_theme::resolve(&t, true, &[]),
    ])
}

/// 内置资产 + 调用方资源（同名覆盖内置）登记进本线程的内存资源表。
fn install_assets(assets: Vec<(String, Vec<u8>)>) {
    let mut m = wind_scene::assets::MemoryAssets::new();
    crate::builtin::add_assets(&mut m);
    for (path, bytes) in assets {
        m.insert(&path, bytes);
    }
    wind_scene::assets::install(m);
}

/// 事件名 → 装饰事件与目标装饰：`typing` / `commit` / `idle_5s` …，`click` 给全部装饰，
/// `click:2` 只给第 2 块。
fn parse_signal(name: &str) -> Option<(DecorEvent, Option<usize>)> {
    if let Some(i) = name.strip_prefix("click:") {
        return Some((DecorEvent::Click, Some(i.parse().ok()?)));
    }
    Some((DecorEvent::parse(name)?, None))
}

impl<C: Canvas> Preview<C> {
    /// 预览内置 `default` 主题、只有内置资产。
    pub fn new(canvas: C) -> Self {
        let themes = crate::builtin::theme_text(crate::builtin::DEFAULT_THEME)
            .and_then(|t| resolve_text(&t).ok())
            .unwrap_or_default();
        install_assets(Vec::new());
        Self {
            text: CanvasText::new(canvas),
            themes,
            scene: CandidateScene::default(),
            scene_opts: CandidateSceneOptions::default(),
            visible: false,
            anim_allowed: false,
            last: RenderOpts::default(),
        }
    }

    /// 换主题与资源（路径 → 字节，相对主题目录，如 `assets/panel.png`）。解析失败返回错误
    /// 原文（含 TOML 行列），当前主题与资源不变。
    pub fn set_theme(&mut self, toml: &str, assets: Vec<(String, Vec<u8>)>) -> Result<(), String> {
        self.themes = resolve_text(toml)?;
        install_assets(assets);
        // 新场景：皮肤告警重新报、装饰状态机重建；旧主题的动画帧丢掉。
        self.scene = CandidateScene::default();
        self.visible = false;
        wind_scene::view::clear_anim_frames();
        Ok(())
    }

    /// 按参数搭一帧的场景输入交给 `f`。
    fn with_input<R>(
        &mut self,
        o: &RenderOpts,
        f: impl FnOnce(&mut CandidateScene, &CandidateSceneInput) -> R,
    ) -> Result<R, String> {
        if !(o.scale.is_finite() && o.scale > 0.0 && o.scale <= 8.0) {
            return Err(format!("缩放比例无效：{}", o.scale));
        }
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
        let labels = &src.views.index_labels;
        let candidates: Vec<CandidateItem> = if o.candidates.is_empty() {
            wind_ui_types::preview_candidates(o.per_page, |i| {
                wind_ui_types::theme_index_label(labels, i)
            })
        } else {
            o.candidates
                .iter()
                .enumerate()
                .map(|(i, c)| CandidateItem {
                    text: c.text.clone(),
                    text_styled: Default::default(),
                    code: String::new(),
                    label: if c.label.is_empty() {
                        wind_ui_types::theme_index_label(labels, i)
                    } else {
                        c.label.clone()
                    },
                    tooltip: Default::default(),
                    comment: c.comment.clone().into(),
                    comment_above: Default::default(),
                    no_index: false,
                })
                .collect()
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
            screen_extent_px: None,
            text_renderer: &self.text,
            opts: &self.scene_opts,
        };
        Ok(f(&mut self.scene, &inp))
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

    /// 画一帧。第一帧（或 `show` 之后）各装饰状态机从 `t_ms` 起从头播。
    pub fn render(&mut self, o: &RenderOpts) -> Result<Frame, String> {
        let clock = self.clock(o, o.t_ms);
        let soft_shadow = o.soft_shadow;
        let frame = self.with_input(o, |scene, inp| {
            render_candidate(scene, inp, clock, soft_shadow)
        })?;
        self.visible = true;
        self.last = o.clone();
        Ok(Frame {
            width: frame.width,
            height: frame.height,
            rgba: frame.to_rgba(),
            next_ms: self.scene.next_anim_at().map(|d| d.as_secs_f64() * 1000.0),
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
        let changed =
            self.with_input(&last, |scene, inp| scene.decor_event(inp, ev, only, clock))?;
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

    #[test]
    fn default_theme_renders_sample_candidates() {
        let (mut p, c) = preview();
        let f = p.render(&RenderOpts::default()).unwrap();
        assert_eq!(f.rgba.len(), (f.width * f.height * 4) as usize);
        assert!(f.width > f.height, "横排是宽条：{}x{}", f.width, f.height);
        assert!(opaque_px(&f) > 0, "窗口底色应画出");
        let draws = c.draws.borrow();
        for want in ["你好", "拟好", "wqiy", "nihao"] {
            assert!(draws.iter().any(|(t, _, _)| t == want), "{want}: {draws:?}");
        }
        assert!(
            !draws.iter().any(|(t, _, _)| t == "匿"),
            "出厂 7 条，不画第 10 条"
        );
        assert_eq!(f.next_ms, None, "无皮肤无动画");
    }

    #[test]
    fn opts_parse_from_partial_json_and_change_layout() {
        let (mut p, c) = preview();
        let o: RenderOpts = serde_json::from_str(
            r#"{"vertical":true,"scale":2,"candidates":[{"text":"甲"},{"text":"乙","comment":"注","label":"b"}],"preedit":"jy"}"#,
        )
        .unwrap();
        let v = p.render(&o).unwrap();
        assert!(v.height > v.width, "竖排是高条：{}x{}", v.width, v.height);
        let draws = c.draws.borrow();
        for want in ["甲", "乙", "注", "b", "jy"] {
            assert!(draws.iter().any(|(t, _, _)| t == want), "{want}: {draws:?}");
        }
        assert!(
            draws
                .iter()
                .any(|(_, f, _)| f.contains(" 32px ") || f.contains(" 28px ")),
            "×2 缩放的字号：{draws:?}"
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
        assert!(bare.width <= light.width && bare.height <= light.height);
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
        let draws = c.draws.borrow();
        let f = &draws.iter().find(|(t, _, _)| t == "你好").unwrap().1;
        assert!(f.contains("\"Noto Serif SC\""), "{f}");
        assert!(f.starts_with("700 "), "{f}");
    }

    #[test]
    fn set_theme_errors_verbatim_and_keeps_current_theme() {
        let (mut p, _) = preview();
        let before = p.render(&RenderOpts::default()).unwrap().rgba;
        let e = p.set_theme("[meta\nname = 1", Vec::new()).unwrap_err();
        assert!(e.contains("parse theme"), "{e}");
        let e = p.set_theme("base = \"nope\"\n", Vec::new()).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        assert_eq!(p.render(&RenderOpts::default()).unwrap().rgba, before);
        assert!(
            p.render(&RenderOpts {
                scale: 0.0,
                ..Default::default()
            })
            .is_err()
        );
    }

    /// 继承内置主题、背景图来自调用方的资源表：底图的颜色出现在帧里。
    #[test]
    fn theme_with_base_and_memory_asset() {
        let (mut p, _) = preview();
        let toml = "base = \"default\"\n[meta]\nname = \"t\"\n\
                    [window]\nbackground = { image = { ref = \"assets/bg.png\", mode = \"stretch\" } }\n";
        p.set_theme(
            toml,
            vec![("assets/bg.png".into(), png(4, 4, [0, 200, 0, 255]))],
        )
        .unwrap();
        let f = p.render(&RenderOpts::default()).unwrap();
        let green = f
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| **px == [0, 200, 0, 255])
            .count();
        assert!(green > 100, "底图应铺开：{green}");
    }

    /// 皮肤动画：装饰按 `t_ms` 换帧，`next_ms` 报下一次换帧；`signal` 认得事件名。
    #[test]
    fn skin_animation_follows_t_ms_and_signals() {
        let (mut p, _) = preview();
        let toml = "[meta]\nname = \"s\"\n[skin]\ndensity = 1\n\
                    [skin.horizontal.frame]\nimage = \"frame.png\"\n\
                    stretch_x = [[4, 12]]\nstretch_y = [[4, 12]]\n\
                    [[skin.horizontal.decor]]\nimage = \"dot.gif\"\nanchor = \"frame.top_left\"\n";
        let gif = gif2([255, 0, 0, 255], [0, 0, 255, 255]);
        p.set_theme(
            toml,
            vec![
                ("frame.png".into(), png(16, 16, [200, 200, 200, 255])),
                ("dot.gif".into(), gif),
            ],
        )
        .unwrap();
        let at = |p: &mut Preview<MockCanvas>, t: f64| {
            p.render(&RenderOpts {
                animate: true,
                t_ms: t,
                ..Default::default()
            })
            .unwrap()
        };
        let f0 = at(&mut p, 0.0);
        assert_eq!(f0.next_ms, Some(100.0), "两帧各 100ms");
        let f1 = at(&mut p, 150.0);
        assert_ne!(f0.rgba, f1.rgba, "第二帧换了颜色");
        let f2 = at(&mut p, 250.0);
        assert_eq!(f0.rgba, f2.rgba, "循环回第一帧");
        // 不播动画：停在首帧，不报换帧。
        let still = p.render(&RenderOpts::default()).unwrap();
        assert_eq!(still.next_ms, None);
        assert!(p.signal("typing", None).is_ok());
        assert!(p.signal("click:0", Some(10.0)).is_ok());
        assert!(p.signal("show", None).unwrap());
        assert!(p.signal("nonsense", None).is_err());
        assert!(p.signal("click:x", None).is_err());
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
