//! 图片帧信息与缩略图（模块级、无状态）：编辑器的动画缩略条用，帧号、帧数与时长按引擎口径
//! （`wind_scene::image_cache::walk_frames`：同一个解码器、同一套限额、≥ 2 帧才算动画、时长归一）。
//!
//! 输入不可信：字节、缩略边长、一次取的帧数都设上限；解码本身受引擎位图上限约束。

use serde::{Deserialize, Serialize};

/// 输入字节上限（同主题包单图上限）。
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// 缩略图最长边上限（像素）。
pub const MAX_THUMB_EDGE: u32 = 256;
/// 一次至多取这么多帧。
pub const MAX_THUMB_FRAMES: usize = 64;
/// 一次调用的解码量预算：画布像素 × 解的帧数（帧已合成到整张画布，取 `[from, to)` 也要从第 0 帧解起）。
/// 384 Mi 约是 1024² 的画布解满引擎的 300 帧；`inspectImage` 超了只报已解出的帧并标 `over_budget`，
/// `decodeFrames` 超了报错。
/// 不设它时，16 MiB 内就能塞下 8192×4096 × 300 帧的纯色 APNG（单次解码量约 40 GB 像素字节）。
pub const MAX_DECODE_PX_FRAMES: u64 = 384 * 1024 * 1024;

/// 一帧的时长。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrameMs {
    /// 引擎归一后的时长（毫秒，不计装饰的 `fps` 覆盖）；静态图为 0。
    pub ms: u32,
}

/// `inspect_image` 的结果（JSON 键 camelCase：`overBudget`）。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    /// `png` / `jpeg` / `webp` / `gif` / `bmp`。
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    /// 引擎按动画播；否则只画首帧（`frames` 只有一项）。
    pub animated: bool,
    /// 各帧（下标即引擎原始帧号，`states` 的帧区间按它写）。
    pub frames: Vec<FrameMs>,
    /// `frames` 不全：帧数超过引擎的最大解帧数（其后的帧引擎也不解），或解码量超了
    /// [`MAX_DECODE_PX_FRAMES`]（此时 `over_budget`）。
    pub truncated: bool,
    /// 解码量超预算：`frames` 只有超之前解出的那些（可为空），之后的没解；没解到第 1 帧时
    /// `animated` 按文件头判断。
    pub over_budget: bool,
}

/// 字节数在上限内。
fn check_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "图片过大：{} 字节（上限 {MAX_IMAGE_BYTES}）",
            bytes.len()
        ));
    }
    Ok(())
}

/// 格式、尺寸、帧数与各帧时长。
pub fn inspect_image(bytes: &[u8]) -> Result<ImageInfo, String> {
    inspect_within(bytes, MAX_DECODE_PX_FRAMES)
}

fn inspect_within(bytes: &[u8], budget: u64) -> Result<ImageInfo, String> {
    check_bytes(bytes)?;
    let (format, width, height) = wind_scene::image_cache::image_header(bytes)?;
    let mut frames = Vec::new();
    let walk = wind_scene::image_cache::walk_frames(bytes, budget, |_, _, _, _, ms| {
        frames.push(FrameMs { ms });
        true
    })?;
    Ok(ImageInfo {
        format,
        width,
        height,
        animated: walk.animated,
        frames,
        truncated: walk.truncated || walk.over_budget,
        over_budget: walk.over_budget,
    })
}

/// `decode_frames` 的参数（JSON，字段全可省）。
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ThumbOpts {
    /// 缩略图最长边（像素，1..=[`MAX_THUMB_EDGE`]）；原图更小时不放大。
    pub max_edge: u32,
    /// 起始帧号（含，原始帧号）。
    pub from: usize,
    /// 结束帧号（不含）；None = `from` + [`MAX_THUMB_FRAMES`]。
    pub to: Option<usize>,
}

impl Default for ThumbOpts {
    fn default() -> Self {
        Self {
            max_edge: 64,
            from: 0,
            to: None,
        }
    }
}

/// 一张缩略帧：直通 alpha 的 RGBA（`w × h × 4`）。
#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    /// 原始帧号。
    pub index: usize,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub ms: u32,
}

/// 预乘 → 直通 alpha。
fn unpremultiply(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        match a {
            255 => {}
            0 => p[..3].fill(0),
            _ => {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
}

/// `[from, to)` 各帧的缩略图（面积平均，同引擎缩放），越过末帧的部分不返回。
pub fn decode_frames(bytes: &[u8], o: &ThumbOpts) -> Result<Vec<Thumb>, String> {
    decode_within(bytes, o, MAX_DECODE_PX_FRAMES)
}

fn decode_within(bytes: &[u8], o: &ThumbOpts, budget: u64) -> Result<Vec<Thumb>, String> {
    check_bytes(bytes)?;
    if !(1..=MAX_THUMB_EDGE).contains(&o.max_edge) {
        return Err(format!(
            "缩略边长无效：{}（须在 1..={MAX_THUMB_EDGE}）",
            o.max_edge
        ));
    }
    let to = o.to.unwrap_or(o.from.saturating_add(MAX_THUMB_FRAMES));
    if to < o.from || to - o.from > MAX_THUMB_FRAMES {
        return Err(format!(
            "帧区间无效：[{}, {to})（一次至多 {MAX_THUMB_FRAMES} 帧）",
            o.from
        ));
    }
    let mut out = Vec::new();
    if to == o.from {
        return Ok(out);
    }
    // 只解到 `to` 为止（回调返回 false 即停）；之前的帧也得解（合成），一并计入预算。
    let walk = wind_scene::image_cache::walk_frames(bytes, budget, |i, w, h, rgba, ms| {
        if i >= o.from {
            let f = (o.max_edge as f32 / w.max(h) as f32).min(1.0);
            let (tw, th) = (
                ((w as f32 * f).round() as u32).max(1),
                ((h as f32 * f).round() as u32).max(1),
            );
            let px = if (tw, th) == (w, h) {
                rgba.to_vec()
            } else {
                let mut px = wind_scene::skin::resample(rgba, w, h, tw, th);
                unpremultiply(&mut px);
                px
            };
            out.push(Thumb {
                index: i,
                w: tw,
                h: th,
                rgba: px,
                ms,
            });
        }
        i + 1 < to
    })?;
    if walk.over_budget {
        let (_, w, h) = wind_scene::image_cache::image_header(bytes)?;
        return Err(format!(
            "解码量超出预算：{w}×{h} 的画布解到第 {to} 帧要超过 {budget} 像素·帧"
        ));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
