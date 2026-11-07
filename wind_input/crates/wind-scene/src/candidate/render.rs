//! 候选窗一次出图：不定位、不接鼠标，只要像素（设置端预览、主题编辑器用）。

use super::{AnimClock, CandidateScene, CandidateSceneInput};
use crate::text::SizedText;

/// 一帧候选窗位图。
pub struct CandidateFrame {
    pub width: u32,
    pub height: u32,
    /// 预乘 alpha 的 BGRA（`width × height × 4`），画软件阴影时含其扩边。
    pub bgra: Vec<u8>,
}

impl CandidateFrame {
    /// 转成直通 alpha 的 RGBA（PNG、`<canvas>` 等要的格式）。
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.bgra.clone();
        for px in out.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            let a = px[3] as u32;
            match a {
                255 => {}
                // 全透明像素的 RGB 无意义，归零。
                0 => px[..3].fill(0),
                // 预乘值理应 ≤ a，个别路径写出过 c > a 的像素，钳到 255。
                _ => {
                    for c in &mut px[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        out
    }
}

/// 按输入画一帧候选窗，走与实时窗口同一条排版与绘制（不拆穿透装饰）。
///
/// `soft_shadow` = 画不画软件阴影，与实机对齐：Windows 本地窗口与宿主渲染画（传 true）；
/// macOS / Linux 外部宿主（ext_presenter）由宿主出系统阴影、服务端不画（传 false，扩边随之为 0）。
///
/// 文字取 `inp.text_renderer`，基准字号钉成 `base_font_logical × scale`（见 [`SizedText`]）。
/// `inp.above` 为真时按上方专属布局重排（实时窗口由定位决定，这里由调用方指定）。
///
/// `scene` 由调用方持有、跨次复用：皮肤告警按它去重（每次新建则同一条每张图都报一遍）。
/// 装饰状态机也留在里面——`clock.allowed` 为假时各装饰取初始状态首帧，与状态机无关；
/// 要播动画的调用方换皮肤时须像实时窗口的 `set_theme` 那样复位 `decor_anims_key`。
pub fn render_candidate(
    scene: &mut CandidateScene,
    inp: &CandidateSceneInput,
    clock: AnimClock,
    soft_shadow: bool,
) -> CandidateFrame {
    let text = SizedText {
        inner: inp.text_renderer,
        base_size: inp.base_font_logical * inp.scale,
    };
    let below = CandidateSceneInput {
        above: false,
        text_renderer: &text,
        ..*inp
    };
    let mut frame = scene.layout(&below, clock, soft_shadow);
    if inp.above {
        let above = CandidateSceneInput {
            above: true,
            ..below
        };
        scene.relayout_above(&above, &mut frame);
    }
    let (width, height) = frame.size();
    let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
    scene.paint(&frame, &mut bgra, &text, false);
    CandidateFrame {
        width,
        height,
        bgra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateSceneOptions;
    use crate::text::MockText;
    use wind_ui_types::CandidateItem;

    fn cand(text: &str) -> CandidateItem {
        CandidateItem {
            text: text.to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    #[test]
    fn renders_default_theme_frame_with_mock_text() {
        let mut theme = wind_theme::Resolved::default();
        theme.views.shadow_blur = Some(wind_theme::schema::Dim::Dp(8.0));
        theme.views.shadow_color = Some([0, 0, 0, 64]);
        let text = MockText::new("", 99.0).unwrap();
        let opts = CandidateSceneOptions::default();
        let cands = [cand("你好"), cand("世界")];
        let inp = CandidateSceneInput {
            candidates: &cands,
            preedit: "nihao",
            preedit_caret: 5,
            mode_label: "",
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            vertical: false,
            rotated: false,
            upright: false,
            preedit_embedded: false,
            above: false,
            theme: &theme,
            base_font_logical: 16.0,
            scale: 1.0,
            screen_extent_px: Some((1920, 1080)),
            text_renderer: &text,
            opts: &opts,
        };
        let clock = AnimClock {
            t: std::time::Duration::ZERO,
            showing: true,
            allowed: false,
        };
        let mut scene = CandidateScene::default();
        let f = render_candidate(&mut scene, &inp, clock, true);
        assert!(f.width >= 40 && f.height >= 24, "{}x{}", f.width, f.height);
        assert_eq!(f.bgra.len(), (f.width * f.height * 4) as usize);
        assert!(
            f.bgra.as_chunks::<4>().0.iter().any(|p| p[3] == 255),
            "窗口底色应画出"
        );
        // 候选文字按场景字号（16 × 1.0）画，而不是后端自带的 99。
        let log = text.take_draw_log();
        assert!(log.iter().any(|l| l.contains("\"你好\"")), "{log:?}");
        assert!(!log.iter().any(|l| l.contains("size=99.0")), "{log:?}");
        // 同输入出同一帧。
        assert_eq!(render_candidate(&mut scene, &inp, clock, true).bgra, f.bgra);
        assert_eq!(f.to_rgba().len(), f.bgra.len());
        // 不画软件阴影（ext_presenter 形态）：没有扩边，缓冲恰为内容盒，严格更小。
        let bare = render_candidate(&mut scene, &inp, clock, false);
        assert!(bare.width < f.width && bare.height < f.height);
    }

    /// 带皮肤的临时主题：16×16 窗体 + 一块装饰（`decor` 是装饰表的额外字段）。
    fn skin_theme(tag: &str, decor: &str) -> (std::path::PathBuf, wind_theme::Resolved) {
        let dir = std::env::temp_dir().join(format!(
            "wind_scene_render_limits_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let t = dir.join("t");
        std::fs::create_dir_all(&t).unwrap();
        let px = |w, h| image::RgbaImage::from_pixel(w, h, image::Rgba([200, 200, 200, 255]));
        px(16, 16).save(t.join("frame.png")).unwrap();
        px(4, 4).save(t.join("dot.png")).unwrap();
        std::fs::write(
            t.join("theme.toml"),
            format!(
                "[meta]\nname = \"t\"\n[skin]\ndensity = 1\n\
                 [skin.horizontal.frame]\nimage = \"frame.png\"\n\
                 stretch_x = [[4, 12]]\nstretch_y = [[4, 12]]\n\
                 [[skin.horizontal.decor]]\nimage = \"dot.png\"\nanchor = \"frame.top_left\"\n{decor}\n"
            ),
        )
        .unwrap();
        let theme = wind_theme::load_resolved(&dir, "t", false).unwrap();
        assert!(theme.skin.is_some());
        (dir, theme)
    }

    fn render_with(theme: &wind_theme::Resolved) -> (CandidateFrame, bool) {
        let text = MockText::new("", 16.0).unwrap();
        let opts = CandidateSceneOptions::default();
        let cands = [cand("你好")];
        let inp = CandidateSceneInput {
            candidates: &cands,
            preedit: "nihao",
            preedit_caret: 5,
            mode_label: "",
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            vertical: false,
            rotated: false,
            upright: false,
            preedit_embedded: false,
            above: false,
            theme,
            base_font_logical: 16.0,
            scale: 1.0,
            screen_extent_px: Some((1920, 1080)),
            text_renderer: &text,
            opts: &opts,
        };
        let clock = AnimClock {
            t: std::time::Duration::ZERO,
            showing: true,
            allowed: false,
        };
        let mut scene = CandidateScene::default();
        let f = render_candidate(&mut scene, &inp, clock, false);
        (f, scene.skin_geom.is_some())
    }

    #[test]
    fn hostile_skin_geometry_stays_within_image_limits() {
        // 正常的装饰：皮肤照常启用。
        let (d0, ok) = skin_theme("ok", "offset = [2, 2]");
        assert!(render_with(&ok).1, "正常皮肤应启用");
        // 偏移大到外扩撑破上限：整套回退普通窗口，缓冲是普通尺寸。
        let (d1, far) = skin_theme("far", "offset = [100000000, 0]");
        let (f, skinned) = render_with(&far);
        assert!(!skinned, "外扩超限应回退");
        assert!(
            crate::skin::fits_image_limits(f.width, f.height),
            "{}x{}",
            f.width,
            f.height
        );
        // 声明巨大尺寸的装饰：按比例钳进上限，不去分配那么大的位图。
        let (d2, huge) = skin_theme("huge", "size = [1000000, 1000000]");
        let (f, _) = render_with(&huge);
        assert!(
            crate::skin::fits_image_limits(f.width, f.height),
            "{}x{}",
            f.width,
            f.height
        );
        for d in [d0, d1, d2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn to_rgba_swaps_channels_and_unpremultiplies() {
        let f = CandidateFrame {
            width: 3,
            height: 1,
            bgra: vec![10, 20, 30, 255, 64, 32, 16, 128, 9, 9, 9, 0],
        };
        assert_eq!(
            f.to_rgba(),
            vec![30, 20, 10, 255, 32, 64, 128, 128, 0, 0, 0, 0]
        );
    }
}
