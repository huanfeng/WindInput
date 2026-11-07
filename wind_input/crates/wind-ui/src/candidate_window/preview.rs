//! 设置端主题预览（`UiCommand::RenderCandidatePreview`）：用实时窗口的文字后端与外观覆盖，
//! 按请求里的主题、横竖与缩放画一帧示例候选窗，编码成 PNG。不定位、不上屏、不播动画。

use super::CandidateWindow;
use super::scene::{self, AnimClock, CandidateSceneInput};
use wind_ui_types::{CandidatePreviewImage, CandidatePreviewRequest};

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
        let frame =
            scene::render_candidate(&mut self.preview_scene, &inp, clock, !cfg!(ext_presenter));
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
    fn preview_rejects_bad_scale() {
        let mut w = new_window();
        let (req, _rx) = request(wind_theme::Resolved::default(), false, 0.0);
        assert!(w.render_preview(&req).is_err());
    }
}
