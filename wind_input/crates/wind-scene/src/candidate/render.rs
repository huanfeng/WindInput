//! 候选窗一次出图：不定位、不接鼠标，只要像素（设置端预览、主题编辑器用）。

use super::{AnimClock, CandidateScene, CandidateSceneInput, NodeKey};
use crate::text::SizedText;
use crate::view::Rect;

/// 一帧候选窗位图。
pub struct CandidateFrame {
    pub width: u32,
    pub height: u32,
    /// 预乘 alpha 的 BGRA（`width × height × 4`），画软件阴影时含其扩边。
    pub bgra: Vec<u8>,
    /// 各节点的矩形（设备像素，同 `bgra` 的坐标），供编辑器预览点选（见 [`CandidateScene::nodes`]）。
    pub nodes: Vec<(NodeKey, Rect)>,
    /// 屏幕安全宽把窗口钳窄了（[`super::SceneFrame::safety_clamped`]）。
    pub safety_clamped: bool,
    /// 本帧皮肤几何（同 `bgra` 坐标）：只有 [`render_candidate_within`]（主题编辑器）取；
    /// None = 没取，或没启用皮肤（含回退普通窗口）。
    pub skin: Option<super::SkinSnapshot>,
}

/// 预乘 BGRA 绘制缓冲 → 直通 RGBA（PNG、`<canvas>` 等通用格式）。
///
/// 窗口绘制缓冲按 `UpdateLayeredWindow`（`AC_SRC_ALPHA`）的约定维护为**预乘 BGRA**；
/// 直接当 RGBA 编码会把 R/B 交换、把预乘值当直通值，半透明像素被系统性压暗偏色
/// （见 `screenshot::unpremultiply` 的文档）。候选窗预览（[`CandidateFrame::to_rgba`]）
/// 与工具栏预览（wind-ui `render_toolbar_preview`）出图共用这一份，格式转换不抄第二处。
pub fn premultiplied_bgra_to_rgba(mut buf: Vec<u8>) -> Vec<u8> {
    for px in buf.as_chunks_mut::<4>().0 {
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
    buf
}

impl CandidateFrame {
    /// 转成直通 alpha 的 RGBA（PNG、`<canvas>` 等要的格式）。
    pub fn to_rgba(&self) -> Vec<u8> {
        premultiplied_bgra_to_rgba(self.bgra.clone())
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
    let frame = layout_frame(scene, inp, clock, soft_shadow, &text);
    paint_frame(scene, inp, &frame, &text, false)
}

/// 同 [`render_candidate`]，但排版出来的缓冲宽或高超过 `max_side`、或像素总数超过
/// `max_pixels` 时不画、返回错误（含尺寸）。给参数与主题都不可信的调用方用（主题编辑器）：
/// 超大字号、海量候选在分配缓冲与逐像素绘制之前就挡住。另带皮肤几何快照（`skin`，编辑器叠加层用；
/// [`render_candidate`] 不取）。
pub fn render_candidate_within(
    scene: &mut CandidateScene,
    inp: &CandidateSceneInput,
    clock: AnimClock,
    soft_shadow: bool,
    max_side: u32,
    max_pixels: u64,
) -> Result<CandidateFrame, String> {
    let text = SizedText {
        inner: inp.text_renderer,
        base_size: inp.base_font_logical * inp.scale,
    };
    let frame = layout_frame(scene, inp, clock, soft_shadow, &text);
    let (w, h) = frame.size();
    if w > max_side || h > max_side || u64::from(w) * u64::from(h) > max_pixels {
        return Err(format!(
            "候选窗尺寸 {w}×{h} 超出上限（单边 {max_side}、共 {max_pixels} 像素）"
        ));
    }
    Ok(paint_frame(scene, inp, &frame, &text, true))
}

/// 排版（含上方专属布局的重排）。`text` 是钉好基准字号的文字后端。
fn layout_frame(
    scene: &mut CandidateScene,
    inp: &CandidateSceneInput,
    clock: AnimClock,
    soft_shadow: bool,
    text: &SizedText,
) -> super::SceneFrame {
    let below = CandidateSceneInput {
        above: false,
        text_renderer: text,
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
    frame
}

fn paint_frame(
    scene: &mut CandidateScene,
    inp: &CandidateSceneInput,
    frame: &super::SceneFrame,
    text: &SizedText,
    with_skin: bool,
) -> CandidateFrame {
    let (width, height) = frame.size();
    let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
    scene.paint(frame, &mut bgra, text, false);
    CandidateFrame {
        width,
        height,
        bgra,
        nodes: scene.nodes(frame),
        safety_clamped: frame.safety_clamped.is_some(),
        skin: with_skin
            .then(|| {
                let (g, s) = scene.skin_geom.as_ref().zip(inp.theme.skin.as_ref())?;
                g.snapshot(s)
            })
            .flatten(),
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
