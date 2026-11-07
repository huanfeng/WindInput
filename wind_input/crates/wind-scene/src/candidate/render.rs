//! 候选窗一次出图：不定位、不接鼠标，只要像素（设置端预览、主题编辑器用）。

use super::{AnimClock, CandidateScene, CandidateSceneInput};
use crate::text::SizedText;

/// 一帧候选窗位图。
pub struct CandidateFrame {
    pub width: u32,
    pub height: u32,
    /// 预乘 alpha 的 BGRA（`width × height × 4`），含软件阴影扩边。
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

/// 按输入画一帧候选窗，走与实时窗口同一条排版与绘制（软件阴影照画，不拆穿透装饰）。
///
/// 文字取 `inp.text_renderer`，基准字号钉成 `base_font_logical × scale`（见 [`SizedText`]）。
/// `inp.above` 为真时按上方专属布局重排（实时窗口由定位决定，这里由调用方指定）。
/// 每次用一个新场景：装饰动画从 `clock` 给的时刻取帧，没有跨帧状态。
pub fn render_candidate(inp: &CandidateSceneInput, clock: AnimClock) -> CandidateFrame {
    let text = SizedText {
        inner: inp.text_renderer,
        base_size: inp.base_font_logical * inp.scale,
    };
    let below = CandidateSceneInput {
        above: false,
        text_renderer: &text,
        ..*inp
    };
    let mut scene = CandidateScene::default();
    let mut frame = scene.layout(&below, clock, true);
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
        let theme = wind_theme::Resolved::default();
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
        let f = render_candidate(&inp, clock);
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
        assert_eq!(render_candidate(&inp, clock).bgra, f.bgra);
        assert_eq!(f.to_rgba().len(), f.bgra.len());
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
