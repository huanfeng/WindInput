//! 设置端主题预览（`UiCommand::RenderCandidatePreview`）：用实时窗口的文字后端与外观覆盖，
//! 按请求里的主题、横竖与缩放画一帧示例候选窗，编码成 PNG。不定位、不上屏、不播动画。
//!
//! 预览从不和打字抢 UI 线程：请求先进 [`PreviewQueue`]，消息循环只在命令通道排空的那一轮
//! 画**一张**（见 [`PreviewQueue::take_if_idle`]），请求方已放弃的（`cancel` 置位）直接丢掉。

use std::collections::VecDeque;

use super::CandidateWindow;
use super::scene::{self, AnimClock, CandidateSceneInput};
use wind_ui_types::{
    CandidatePreviewImage, CandidatePreviewRequest, PreviewAppearance, PreviewKind,
};

/// 渲染器的全局字体状态（草稿字体临时换上前的旧值）。
#[derive(PartialEq)]
struct RendererFont {
    family: String,
    weight: i32,
    plan: crate::text::script::FontPlan,
}

/// UI 线程本地的预览待画队列。
#[derive(Default)]
pub struct PreviewQueue {
    q: VecDeque<Box<CandidatePreviewRequest>>,
}

impl PreviewQueue {
    pub fn push(&mut self, req: Box<CandidatePreviewRequest>) {
        self.q.push_back(req);
    }

    /// 还有没画的（含已放弃、待丢的）。
    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    /// 本轮没有别的命令（`had_cmd` 为假）时取一张还有人等的请求；已放弃的顺手丢掉。
    /// 有命令时什么也不给：先把打字等命令处理完，下一轮再说。
    pub fn take_if_idle(&mut self, had_cmd: bool) -> Option<Box<CandidatePreviewRequest>> {
        if had_cmd {
            return None;
        }
        while let Some(req) = self.q.pop_front() {
            if !req.cancelled() {
                return Some(req);
            }
        }
        None
    }

    /// [`Self::take_if_idle`] 取一张、画完回执。返回画没画。
    pub fn render_one(&mut self, had_cmd: bool, w: &mut CandidateWindow) -> bool {
        let Some(req) = self.take_if_idle(had_cmd) else {
            return false;
        };
        // 回执通道容量 1、只回一次，try_send 不会阻塞；对端已超时走掉则丢弃。
        let _ = req.reply.try_send(w.render_preview(&req));
        true
    }
}

impl CandidateWindow {
    /// 画一帧预览。外观覆盖（字体、字号、阴影开关、尺寸下限、翻页栏等）取本窗口当前状态，
    /// 与 [`Self::refresh_effective_theme`] 同一个合并函数；主题、横竖、缩放取请求。
    ///
    /// 软件阴影按本平台实机：ext_presenter（macOS / Linux 外部宿主）不画。场景用
    /// [`Self::preview_scene`]，与实时窗口的场景分开，不扰动后者的跨帧状态。
    pub fn render_preview(
        &mut self,
        req: &CandidatePreviewRequest,
    ) -> Result<CandidatePreviewImage, String> {
        if !(req.scale.is_finite() && req.scale > 0.0) {
            return Err(format!("缩放比例无效：{}", req.scale));
        }
        // fit 参数是正整数（RPC 层挡住 0 与非整数）；这里挡内部 API 直调，防折算时除零歧义。
        if req.fit_w == Some(0) || req.fit_h == Some(0) {
            return Err("fit_w / fit_h 只能是正整数（收到 0）".to_string());
        }
        // 草稿外观：字体先换到渲染器上（全局字族 / 默认字重 / 字体方案是渲染器状态，不在主题里），
        // 画完换回；其余覆盖只进本帧的输入，不碰窗口状态。
        // 工具栏预览不走字体替换：实时工具栏用固定字族字号（[`crate::toolbar::FONT_FAMILY`]），
        // 不随用户字体配置变，预览也不该变。
        let swapped = (req.kind == PreviewKind::Candidate)
            .then(|| {
                req.appearance
                    .as_ref()
                    .and_then(|a| self.swap_preview_font(a))
            })
            .flatten();
        let out = self.render_preview_frame(req);
        if let Some(live) = swapped {
            self.text_renderer.set_font_family(&live.family);
            self.text_renderer.set_default_weight(live.weight);
            self.text_renderer.set_font_plan(live.plan);
        }
        out
    }

    /// 草稿字体与渲染器当前的不同时换成草稿的，返回换回去要的旧值；相同时什么也不做。
    /// 字体名走 [`Self::resolve_font`]（与配置重载同一解析），但不记日志：草稿还没保存。
    fn swap_preview_font(&mut self, a: &PreviewAppearance) -> Option<RendererFont> {
        let font_of = |w: &Self,
                       family: &str,
                       weight: i32,
                       fallback: &[String],
                       scripts: &[(String, Vec<String>)]| {
            let r = w.resolve_font(super::resolve_font_family(family));
            let fallback: Vec<String> = fallback.iter().map(|f| w.resolve_font(f).family).collect();
            let scripts: Vec<(String, Vec<String>)> = scripts
                .iter()
                .map(|(k, chain)| {
                    (
                        k.clone(),
                        chain.iter().map(|f| w.resolve_font(f).family).collect(),
                    )
                })
                .collect();
            RendererFont {
                plan: super::build_font_plan(&r.family, &fallback, &scripts),
                weight: if weight > 0 {
                    weight.clamp(1, 950)
                } else {
                    r.weight
                },
                family: r.family,
            }
        };
        let draft = font_of(
            self,
            &a.font_family,
            a.font_weight,
            &a.font_fallback,
            &a.font_scripts,
        );
        // 换回的是渲染器此刻的原值（不按配置重算：两者万一不一致，也不能让预览改掉实时窗口）。
        let live = RendererFont {
            plan: self.text_renderer.font_plan().clone(),
            weight: self.text_renderer.default_weight(),
            family: self.text_renderer.font_family().to_string(),
        };
        if draft == live {
            return None;
        }
        self.text_renderer.set_font_family(&draft.family);
        self.text_renderer.set_default_weight(draft.weight);
        self.text_renderer.set_font_plan(draft.plan);
        Some(live)
    }

    fn render_preview_frame(
        &mut self,
        req: &CandidatePreviewRequest,
    ) -> Result<CandidatePreviewImage, String> {
        if req.kind == PreviewKind::Toolbar {
            return self.render_toolbar_preview(req);
        }
        let a = req.appearance.as_ref();
        let o = scene::ThemeOverrides {
            vertical: req.vertical,
            user_font_family: a.map_or(&self.user_font_family, |a| &a.font_family),
            user_font_weight: a.map_or(self.user_font_weight, |a| a.font_weight.clamp(0, 950)),
            scheme_text_family: &self.text_family_override,
            shadow: a.map_or(self.theme_shadow, |a| a.shadow),
            font_size: a.map_or(self.font_size_override, |a| a.font_size.max(0.0)),
        };
        let eff = scene::effective_theme(&req.theme, &o, |f| {
            let r = self.resolve_font(f);
            (r.family, r.weight)
        });
        let draft_opts = a.map(|a| scene::CandidateSceneOptions {
            flip_when_above: self.scene_opts.flip_when_above,
            swap_preedit_when_above: self.scene_opts.swap_preedit_when_above,
            pager_in_preedit: a.pager_in_preedit,
            pager_display: a.pager_display.clone(),
            page_number_display: a.page_number_display.clone(),
            min_window_width_horizontal: a.min_size[0],
            min_window_width_vertical: a.min_size[1],
            min_window_height_horizontal: a.min_size[2],
            min_window_height_vertical: a.min_size[3],
            min_rows: a.min_size[4],
        });
        let mut inp = CandidateSceneInput {
            candidates: &req.candidates,
            preedit: &req.preedit,
            preedit_caret: req.preedit.len(),
            mode_label: "",
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 3,
            grid: None,
            vertical: req.vertical,
            rotated: false,
            upright: false,
            preedit_embedded: a.map_or(self.preedit_embedded, |a| a.preedit_embedded),
            above: false,
            theme: &eff.theme,
            base_font_logical: eff.base_font_logical,
            scale: req.scale,
            screen_extent_px: None,
            text_renderer: &self.text_renderer,
            opts: draft_opts.as_ref().unwrap_or(&self.scene_opts),
        };
        let clock = AnimClock {
            t: std::time::Duration::ZERO,
            showing: true,
            allowed: false,
        };
        // fit 折算要先有自然逻辑尺寸：它就是本渲染在同一请求输入下的产出（含草稿尺寸下限、
        // 横竖排与字号等），所以按请求 scale 渲第一遍测出 n；折算出放大（r > 1）时把 scale
        // 提到 scale × r 重渲第二遍——矢量内容高倍率渲染是锐利的，设置端 Contain 后不再
        // 双线性放大。r = 1 时只渲一遍，路径与不带 fit 的旧请求完全一致。
        let mut frame = render_preview_pass(&mut self.preview_scene, &inp, clock);
        let r = wind_ui_types::preview_fit_ratio(
            req.fit_w,
            req.fit_h,
            frame.width as f32 / req.scale,
            frame.height as f32 / req.scale,
        );
        if r > 1.0 {
            inp.scale = req.scale * r;
            frame = render_preview_pass(&mut self.preview_scene, &inp, clock);
        }
        let rgba = frame.to_rgba();
        let mut png = Vec::new();
        {
            use image::ImageEncoder;
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(
                    &rgba,
                    frame.width,
                    frame.height,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| format!("预览图编码失败：{e}"))?;
        }
        Ok(CandidatePreviewImage {
            width: frame.width,
            height: frame.height,
            png,
        })
    }

    /// 工具栏预览（`kind = toolbar`）：配色 / 几何 / 绘制全部走实时工具栏（[`crate::toolbar`]）
    /// 的同一套折算与画笔——`ToolbarSkin` / `ToolbarDims` / `toolbar_frame` / `paint_toolbar`
    /// 均为窗口路径共用，预览不抄第二份。
    ///
    /// 与实时工具栏同源的还有两处：格子展开 [`crate::toolbar::expand_cells`]（含空布局回落）
    /// 与固定字族字号 [`crate::toolbar::FONT_FAMILY`] / [`crate::toolbar::Toolbar::FONT_PX`]
    /// （工具栏不随用户字体配置变）。状态取请求里的 `toolbar_state`，没带就是
    /// [`wind_ui_types::preview_toolbar_state`] 的固定样本——预览不跟真机状态走。
    fn render_toolbar_preview(
        &mut self,
        req: &CandidatePreviewRequest,
    ) -> Result<CandidatePreviewImage, String> {
        let state = req
            .toolbar_state
            .clone()
            .unwrap_or_else(wind_ui_types::preview_toolbar_state);
        let cells = crate::toolbar::expand_cells(&req.toolbar_items, &state);
        let skin = crate::toolbar::ToolbarSkin::from_theme(&req.theme);
        let dims = crate::toolbar::ToolbarDims::from_theme(&req.theme);
        // fit 折算：toolbar_frame 是纯函数，scale = 1 直接给逻辑尺寸 n，不用先渲一遍。
        let n = crate::toolbar::toolbar_frame(&dims, 1.0, req.vertical, cells.len());
        let r = wind_ui_types::preview_fit_ratio(req.fit_w, req.fit_h, n.w as f32, n.h as f32);
        let s = req.scale * r;
        let frame = crate::toolbar::toolbar_frame(&dims, s, req.vertical, cells.len());
        let mut buf = vec![0u8; frame.w as usize * frame.h as usize * 4];
        let renderer = crate::text::dwrite::TextRenderer::new(
            crate::toolbar::FONT_FAMILY,
            crate::toolbar::Toolbar::FONT_PX * s,
        )?;
        // 预防性隔离：toolbar 绘制目前走的路径不进图片/绘制缓存，包上这层是给
        // 将来加缓存上保险——现在没有实际隔离收益，别把它当成已生效的防线。
        crate::view::with_isolated_paint_caches(|| {
            crate::toolbar::paint_toolbar(
                &mut buf,
                frame.w,
                frame.h,
                &frame,
                &cells,
                req.vertical,
                -1, // 预览没有鼠标，无悬停格
                &skin,
                &state,
                &renderer,
            );
        });
        // 出图前换格式：绘制缓冲是预乘 BGRA（窗口/预览同一套 paint_toolbar），直接当
        // RGBA 编码 PNG 会把 R/B 交换、把预乘值当直通值——半透明像素被系统性压暗偏色，
        // 真机反馈的「预览色差」正是这个。转换与候选窗预览的 [`CandidateFrame::to_rgba`]
        // 共用一份（格式转换不抄第二处）。
        let rgba = wind_scene::candidate::premultiplied_bgra_to_rgba(buf);
        let mut png = Vec::new();
        {
            use image::ImageEncoder;
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&rgba, frame.w, frame.h, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("预览图编码失败：{e}"))?;
        }
        Ok(CandidatePreviewImage {
            width: frame.w,
            height: frame.h,
            png,
        })
    }
}

/// 渲一遍候选窗预览帧（按 `inp.scale` 出物理像素）。皮肤告警每遍重报（跨遍去重会让
/// 坏主题在第二遍里无声），绘制缓存隔离——预览不进也不清实时窗口的图片 / 阴影缓存。
/// 拿 `&mut CandidateScene` 而不是窗口：`inp` 里已借了窗口的其它字段（文字后端等），
/// 拿整个窗口会撞借用。
fn render_preview_pass(
    scene_state: &mut super::CandidateScene,
    inp: &CandidateSceneInput,
    clock: AnimClock,
) -> wind_scene::candidate::CandidateFrame {
    scene_state.skin_warned.borrow_mut().clear();
    crate::view::with_isolated_paint_caches(|| {
        scene::render_candidate(scene_state, inp, clock, !cfg!(ext_presenter))
    })
}

#[cfg(all(test, mock_text))]
mod tests {
    use super::*;

    fn cand(text: &str, comment: &str) -> wind_ui_types::CandidateItem {
        wind_ui_types::CandidateItem {
            text: text.to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: comment.to_string().into(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    fn request(
        theme: wind_theme::Resolved,
        vertical: bool,
        scale: f32,
    ) -> (
        CandidatePreviewRequest,
        std::sync::mpsc::Receiver<Result<CandidatePreviewImage, String>>,
    ) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let req = CandidatePreviewRequest {
            theme,
            kind: PreviewKind::Candidate,
            preedit: "nihao".into(),
            candidates: vec![cand("你好", ""), cand("拟好", ""), cand("你", "wqiy")],
            vertical,
            scale,
            fit_w: None,
            fit_h: None,
            toolbar_items: Vec::new(),
            toolbar_state: None,
            appearance: None,
            reply: tx,
            cancel: Default::default(),
        };
        (req, rx)
    }

    fn decode(img: &CandidatePreviewImage) -> image::RgbaImage {
        image::load_from_memory_with_format(&img.png, image::ImageFormat::Png)
            .expect("PNG 能解码")
            .to_rgba8()
    }

    fn new_window() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        CandidateWindow::new(Default::default(), tx).unwrap()
    }

    fn themes_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
    }

    #[test]
    fn preview_png_decodes_with_frame_size_and_style_changes_pixels() {
        let dirs = vec![themes_dir()];
        let light = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let dark = wind_theme::load_resolved_dirs(&dirs, "default", true).unwrap();
        let mut w = new_window();

        let (req, _rx) = request(light, false, 1.0);
        let a = w.render_preview(&req).unwrap();
        let da = decode(&a);
        assert_eq!((da.width(), da.height()), (a.width, a.height));
        assert!(
            a.width > a.height,
            "横排预览应是宽条：{}x{}",
            a.width,
            a.height
        );

        let (req, _rx) = request(dark, false, 1.0);
        let b = w.render_preview(&req).unwrap();
        assert_eq!(
            (a.width, a.height),
            (b.width, b.height),
            "明暗只换色不换几何"
        );
        assert_ne!(da.into_raw(), decode(&b).into_raw(), "明暗两版像素应不同");

        let (req, _rx) = request(req.theme, true, 2.0);
        let v = w.render_preview(&req).unwrap();
        assert!(v.height > a.height, "竖排 ×2 应更高");
    }

    #[test]
    fn queue_yields_to_commands_and_drops_cancelled() {
        use std::sync::atomic::Ordering;
        let mut q = PreviewQueue::default();
        let (a, _ra) = request(wind_theme::Resolved::default(), false, 1.0);
        let (b, rb) = request(wind_theme::Resolved::default(), false, 1.0);
        a.cancel.store(true, Ordering::Release);
        q.push(Box::new(a));
        q.push(Box::new(b));
        assert!(q.take_if_idle(true).is_none(), "有命令待处理时不画");
        assert!(!q.is_empty());
        let mut w = new_window();
        assert!(q.render_one(false, &mut w), "跳过已放弃的，画下一张");
        assert!(rb.try_recv().unwrap().is_ok());
        assert!(q.is_empty());
        assert!(!q.render_one(false, &mut w));
    }

    #[test]
    fn cancelled_request_is_never_rendered() {
        use std::sync::atomic::Ordering;
        let mut q = PreviewQueue::default();
        let (a, ra) = request(wind_theme::Resolved::default(), false, 1.0);
        a.cancel.store(true, Ordering::Release);
        q.push(Box::new(a));
        let mut w = new_window();
        assert!(!q.render_one(false, &mut w));
        assert!(
            matches!(
                ra.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ),
            "丢掉请求即断开回执，不回图"
        );
    }

    fn appearance(font_size: f32, family: &str) -> PreviewAppearance {
        PreviewAppearance {
            font_family: family.into(),
            font_fallback: vec![],
            font_scripts: vec![],
            font_weight: 0,
            font_size,
            shadow: true,
            preedit_embedded: false,
            pager_in_preedit: false,
            pager_display: Default::default(),
            page_number_display: Default::default(),
            min_size: [0; 5],
        }
    }

    /// 草稿外观只作用于这一帧：字号变大图就变大，字体临时换上、画完换回，窗口状态不动。
    #[test]
    fn draft_appearance_applies_to_frame_only() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let (req, _rx) = request(theme.clone(), false, 1.0);
        let live = w.render_preview(&req).unwrap();
        let family = w.text_renderer.font_family().to_string();

        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.appearance = Some(appearance(40.0, "Draft Sans"));
        let big = w.render_preview(&req).unwrap();
        assert!(
            big.height > live.height && big.width > live.width,
            "字号 40 应更大"
        );
        assert_eq!(w.text_renderer.font_family(), family, "字体换回原值");

        let mut tall = appearance(0.0, "");
        tall.min_size = [0, 0, 200, 0, 0];
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.appearance = Some(tall);
        assert!(
            w.render_preview(&req).unwrap().height >= 200,
            "尺寸下限取草稿"
        );

        let (req, _rx) = request(theme, false, 1.0);
        let again = w.render_preview(&req).unwrap();
        assert_eq!((again.width, again.height), (live.width, live.height));
    }

    #[test]
    fn preview_does_not_touch_live_image_cache() {
        let base =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes/_base");
        let live_img = base.join("chevron_prev.svg").to_string_lossy().into_owned();
        let preview_img = base.join("chevron_next.svg").to_string_lossy().into_owned();
        // 实时侧先在缓存里有一条（相当于上一次实时 layout 留下的）。
        assert!(crate::view::image_src_size(&live_img).is_some());
        let before = crate::view::with_image_cache_for_test(|c| c.ids_len_for_test());
        // 预览主题的窗口底图是另一张图：不隔离的话它会进实时缓存。
        let mut theme = wind_theme::Resolved::default();
        theme.views.window.bg_image = Some(wind_theme::RvImage {
            reference: preview_img,
            mode: "stretch".into(),
            opacity: 1.0,
            ..Default::default()
        });
        let mut w = new_window();
        let (req, _rx) = request(theme, false, 1.0);
        w.render_preview(&req).unwrap();
        assert_eq!(
            crate::view::with_image_cache_for_test(|c| c.ids_len_for_test()),
            before,
            "预览不往实时缓存里加东西，也不清它"
        );
        assert!(crate::view::image_src_size(&live_img).is_some());
    }

    #[test]
    fn preview_rejects_bad_scale() {
        let mut w = new_window();
        let (req, _rx) = request(wind_theme::Resolved::default(), false, 0.0);
        assert!(w.render_preview(&req).is_err());
    }

    /// fit 目标框：缺省旧行为不变（与不带 fit 的旧请求同尺寸）；fit = 自然尺寸时倍率 1、
    /// 同图；fit 放大按 Contain 比重渲（封顶 3）；只给一维按该维算；fit 小于自然不缩小；
    /// fit = 0 报错。
    ///
    /// 放大档断言用 ±5% 区间：布局各环节的 ceil / 取整在倍率下有累计量化差（scale_eff=2
    /// 的物理尺寸 ≠ scale=1 基准 × 2 的精确整数倍，实测偏差最大 ~3.7%），5% 是留了余量的
    /// 实测上限——设置端 Contain 只缩小或 1:1，图略大略小不伤锐利度。
    #[test]
    fn fit_scales_output_without_touching_content_layout() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let (base_req, _rx) = request(theme.clone(), false, 1.0);
        let base = w.render_preview(&base_req).unwrap();
        let (bw, bh) = (base.width, base.height);
        assert!(bw > bh);
        let near = |got: u32, want: f32, why: &str| {
            let d = (got as f32 - want).abs() / want;
            assert!(d <= 0.05, "{why}：got {got}，want {want}（偏差 {d:.1}%）");
        };

        // fit = 自然尺寸 → 倍率 1，逐字节同图。
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.fit_w = Some(bw);
        req.fit_h = Some(bh);
        let one = w.render_preview(&req).unwrap();
        assert_eq!((one.width, one.height), (bw, bh), "fit = 自然尺寸 → 倍率 1");
        assert_eq!(
            decode(&one).into_raw(),
            decode(&base).into_raw(),
            "同倍率同图"
        );

        // 两维 ×2 → 出图 ≈2 倍。
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.fit_w = Some(bw * 2);
        req.fit_h = Some(bh * 2);
        let two = w.render_preview(&req).unwrap();
        near(two.width, 2.0 * bw as f32, "fit ×2 → 出图 ≈2 倍（宽）");
        near(two.height, 2.0 * bh as f32, "fit ×2 → 出图 ≈2 倍（高）");

        // 异常大的 fit 封顶 3。
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.fit_w = Some(bw * 10);
        req.fit_h = Some(bh * 10);
        let capped = w.render_preview(&req).unwrap();
        near(capped.width, 3.0 * bw as f32, "封顶 3（宽）");
        near(capped.height, 3.0 * bh as f32, "封顶 3（高）");

        // 只给一维按该维算，另一维随 Contain 同倍率。
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.fit_w = Some(bw * 2);
        let wide = w.render_preview(&req).unwrap();
        near(wide.width, 2.0 * bw as f32, "只给一维（宽）");
        near(wide.height, 2.0 * bh as f32, "只给一维（高）");

        // fit 小于自然尺寸：不缩小，仍 1:1。
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.fit_w = Some(bw / 2);
        req.fit_h = Some(bh / 2);
        let small = w.render_preview(&req).unwrap();
        assert_eq!(
            (small.width, small.height),
            (bw, bh),
            "fit 小于自然 → 不缩小"
        );

        // fit = 0 报错（内部 API 直调的纵深防御；RPC 层同样挡）。
        let (mut req, _rx) = request(theme, false, 1.0);
        req.fit_w = Some(0);
        assert!(w.render_preview(&req).is_err(), "fit = 0 报错");
    }

    fn toolbar_request(theme: wind_theme::Resolved, vertical: bool) -> CandidatePreviewRequest {
        let (mut req, _rx) = request(theme, vertical, 1.0);
        req.kind = PreviewKind::Toolbar;
        req
    }

    /// 工具栏预览按实时工具栏的形状出图：横条是宽条、纵条是高条，画面非空；
    /// 空 items 走 expand_cells 的全集回落，不至于画出一条空带。
    #[test]
    fn toolbar_preview_has_bar_shape_and_content() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let img = w
            .render_preview(&toolbar_request(theme.clone(), false))
            .unwrap();
        assert!(
            img.width > img.height,
            "横条应是宽条：{}x{}",
            img.width,
            img.height
        );
        let d = decode(&img);
        assert_eq!((d.width(), d.height()), (img.width, img.height));
        assert!(d.pixels().any(|p| p.0[3] > 0), "画面应有内容");

        let v = w.render_preview(&toolbar_request(theme, true)).unwrap();
        assert!(v.height > v.width, "纵条应是高条：{}x{}", v.width, v.height);
    }

    /// 工具栏预览不随草稿字体变（实时工具栏用固定字族字号，不随用户字体配置走），
    /// 且与候选窗预览是两种不同的画面。
    #[test]
    fn toolbar_preview_ignores_draft_font_and_differs_from_candidate() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let base = w
            .render_preview(&toolbar_request(theme.clone(), false))
            .unwrap();
        let (mut req, _rx) = request(theme.clone(), false, 1.0);
        req.kind = PreviewKind::Toolbar;
        req.appearance = Some(appearance(40.0, "Draft Sans"));
        let big = w.render_preview(&req).unwrap();
        assert_eq!(
            (big.width, big.height),
            (base.width, base.height),
            "草稿字体不进工具栏预览"
        );

        let (cand_req, _rx) = request(theme, false, 1.0);
        let cand = w.render_preview(&cand_req).unwrap();
        assert_ne!(
            decode(&base).into_raw(),
            decode(&cand).into_raw(),
            "工具栏与候选窗预览应是两种画面"
        );
    }

    /// 工具栏预览同样吃 fit：fit 放大按 toolbar_frame 折算（纯几何，scale 直接进 Dim
    /// 解析），出图 ≈ 目标倍率；画面非空。
    #[test]
    fn toolbar_preview_fit_scales_output() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let base = w
            .render_preview(&toolbar_request(theme.clone(), false))
            .unwrap();
        let (bw, bh) = (base.width, base.height);

        let mut req = toolbar_request(theme, false);
        req.fit_w = Some(bw * 2);
        req.fit_h = Some(bh * 2);
        let two = w.render_preview(&req).unwrap();
        let near = |got: u32, want: f32, why: &str| {
            let d = (got as f32 - want).abs() / want;
            assert!(d <= 0.05, "{why}：got {got}，want {want}（偏差 {d:.1}%）");
        };
        near(two.width, 2.0 * bw as f32, "toolbar fit ×2（宽）");
        near(two.height, 2.0 * bh as f32, "toolbar fit ×2（高）");
        assert!(
            image::load_from_memory_with_format(&two.png, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8()
                .pixels()
                .any(|p| p.0[3] > 0),
            "画面应有内容"
        );
    }

    /// 工具栏预览 PNG 是直通 RGBA：高亮格底色 = 主题 accent 叠底色的合成原色（R/B 不换、
    /// alpha 直通）。
    /// 绘制缓冲是预乘 BGRA（窗口/预览同一套 paint_toolbar），直接当 RGBA 编码会 R/B 交换、
    /// 把预乘值当直通值——设置端按 PNG 直通语义合成后偏暗偏色，真机反馈的「预览色差」
    /// 正是这个（候选窗预览有 to_rgba 转换，工具栏此前漏了）。alpha<255 的反预乘另见
    /// [`toolbar_preview_unpremultiplies_semitransparent_bg`]。
    #[test]
    fn toolbar_preview_png_is_straight_rgba() {
        let dirs = vec![themes_dir()];
        let theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        let mut w = new_window();
        let img = w.render_preview(&toolbar_request(theme, false)).unwrap();
        let d = decode(&img);
        // 高亮格（样本态中文开，Mode 格）。高亮块内缩 inset=4 后为 x 14..40、y 4..26，
        // 取 (20,5)——圆角弧内、避开居中文字。
        // default 主题（继承 _qingfeng）皮肤实测：bg=[255,255,255,247]（半透明白底）、
        // hl_bg=[59,130,246,26]（10% 淡蓝高亮）。高亮格期望 = accent@10% 叠底色后反预乘
        // 的合成原色 ≈ [235,242,254,247]；旧路径（预乘字节当直通写出）给 [228,234,247,247]，
        // 各通道暗 6-7。容差 ±2 同时钉「未压暗」与「R/B 未交换」（交换后 R=254 偏差超 18）。
        let got = d.get_pixel(20, 5).0;
        let want = [235, 242, 254, 247];
        for ch in 0..4 {
            let dv = (got[ch] as i32 - want[ch]).abs();
            assert!(
                dv <= 2,
                "高亮格底色应为主题 accent@10% 合成原色（R/B 未交换、未预乘压暗）：got {got:?} want {want:?}（通道 {ch} 偏差 {dv}）"
            );
        }
        // 常态格底（Punct 格左上角，图标 bbox 外）：半透明白底 α=247 直通写出（白色上
        // R/B 与反预乘都不可辨——247 反预乘恰好整除得 255——此点只钉 alpha 不被涂改）。
        assert_eq!(
            d.get_pixel(46, 5).0,
            [255, 255, 255, 247],
            "常态格底应为主题 bg 直通原色（α=247）"
        );
    }

    /// 半透明底色的反预乘：预乘值（255×128/255=128）当直通值写出会压暗一半；
    /// 正确转换应还原出原色 [255,255,255,128]。
    #[test]
    fn toolbar_preview_unpremultiplies_semitransparent_bg() {
        let dirs = vec![themes_dir()];
        let mut theme = wind_theme::load_resolved_dirs(&dirs, "default", false).unwrap();
        theme.views.toolbar_bg_color = Some([255, 255, 255, 128]);
        let mut w = new_window();
        let img = w.render_preview(&toolbar_request(theme, false)).unwrap();
        let d = decode(&img);
        assert_eq!(
            d.get_pixel(46, 5).0,
            [255, 255, 255, 128],
            "半透明底色应按原 alpha 直通写出，不被预乘压暗"
        );
    }
}
