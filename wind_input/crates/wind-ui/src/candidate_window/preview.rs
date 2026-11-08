//! 设置端主题预览（`UiCommand::RenderCandidatePreview`）：用实时窗口的文字后端与外观覆盖，
//! 按请求里的主题、横竖与缩放画一帧示例候选窗，编码成 PNG。不定位、不上屏、不播动画。
//!
//! 预览从不和打字抢 UI 线程：请求先进 [`PreviewQueue`]，消息循环只在命令通道排空的那一轮
//! 画**一张**（见 [`PreviewQueue::take_if_idle`]），请求方已放弃的（`cancel` 置位）直接丢掉。

use std::collections::VecDeque;

use super::CandidateWindow;
use super::scene::{self, AnimClock, CandidateSceneInput};
use wind_ui_types::{CandidatePreviewImage, CandidatePreviewRequest};

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
        let o = scene::ThemeOverrides {
            vertical: req.vertical,
            user_font_family: &self.user_font_family,
            user_font_weight: self.user_font_weight,
            scheme_text_family: &self.text_family_override,
            shadow: self.theme_shadow,
            font_size: self.font_size_override,
        };
        let eff = scene::effective_theme(&req.theme, &o, |f| {
            let r = self.resolve_font(f);
            (r.family, r.weight)
        });
        let inp = CandidateSceneInput {
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
            preedit_embedded: self.preedit_embedded,
            above: false,
            theme: &eff.theme,
            base_font_logical: eff.base_font_logical,
            scale: req.scale,
            screen_extent_px: None,
            text_renderer: &self.text_renderer,
            opts: &self.scene_opts,
        };
        let clock = AnimClock {
            t: std::time::Duration::ZERO,
            showing: true,
            allowed: false,
        };
        // 每张图各报各的皮肤告警（跨图去重会让第二次预览同一个坏主题时日志里什么也没有）。
        self.preview_scene.skin_warned.borrow_mut().clear();
        // 独立的图片 / 阴影缓存，画完即丢：不清掉实时窗口的源图与图集，临时目录的路径也不留下。
        let scene_state = &mut self.preview_scene;
        let frame = crate::view::with_isolated_paint_caches(|| {
            scene::render_candidate(scene_state, &inp, clock, !cfg!(ext_presenter))
        });
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
            preedit: "nihao".into(),
            candidates: vec![cand("你好", ""), cand("拟好", ""), cand("你", "wqiy")],
            vertical,
            scale,
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
}
