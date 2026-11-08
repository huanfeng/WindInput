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
mod tests {
    use super::*;
    use wind_scene::image_cache::anim_fixtures;

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

    /// 帧数与时长与引擎建帧（`anim_layout`）一致：含一帧 0 ms（归一为 100）、一帧 5 ms（同）、
    /// 一帧 20 ms（钳到 33）。
    #[test]
    fn inspect_matches_engine_anim_layout() {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let gif = anim_fixtures::gif(8, 4, &[(red, 0), (blue, 120), (red, 20), (blue, 5)]);
        let solid = |c: [u8; 4]| c.repeat(8 * 4);
        let apng = anim_fixtures::apng(
            8,
            4,
            &[(solid(red), 0), (solid(blue), 250), (solid(red), 40)],
        );
        for (name, bytes, fmt) in [("a.gif", gif, "gif"), ("a.png", apng, "png")] {
            let info = inspect_image(&bytes).unwrap();
            assert_eq!(
                (info.format, info.width, info.height, info.animated),
                (fmt, 8, 4, true)
            );
            assert!(!info.truncated);
            let mut m = wind_scene::assets::MemoryAssets::new();
            m.insert(name, bytes);
            let m = std::sync::Arc::new(m);
            wind_scene::assets::install(wind_scene::assets::next_generation(), &m);
            let key = wind_scene::assets::source_key(name).unwrap();
            let layout = wind_scene::view::anim_layout(&key, 1.0, 8, 4, None, 1 << 30).unwrap();
            let ms: Vec<u32> = info.frames.iter().map(|f| f.ms).collect();
            assert_eq!(ms, layout.durations, "{name}");
            assert_eq!(info.frames.len(), layout.raw_frames, "{name}");
        }
    }

    #[test]
    fn static_and_single_frame_images_are_not_animated() {
        let info = inspect_image(&png(5, 3, [1, 2, 3, 255])).unwrap();
        assert_eq!(
            (info.format, info.width, info.height, info.animated),
            ("png", 5, 3, false)
        );
        assert_eq!(info.frames, [FrameMs { ms: 0 }]);
        let one = anim_fixtures::gif(4, 4, &[([9, 9, 9, 255], 200)]);
        let info = inspect_image(&one).unwrap();
        assert!(
            !info.animated && info.frames.len() == 1,
            "单帧 GIF 按静态图"
        );
    }

    #[test]
    fn frames_beyond_engine_limit_are_truncated() {
        let n = wind_scene::image_cache::MAX_ANIM_FRAMES + 3;
        let frames: Vec<([u8; 4], u32)> = (0..n)
            .map(|i| ([(i % 256) as u8, 0, 0, 255], 100))
            .collect();
        let info = inspect_image(&anim_fixtures::gif(2, 2, &frames)).unwrap();
        assert!(info.truncated);
        assert_eq!(info.frames.len(), wind_scene::image_cache::MAX_ANIM_FRAMES);
    }

    /// 解码量超预算时 inspect 只报头信息，decode 报错；只解到 `to`，预算内的照常给。
    /// （按比例缩小预算测：真实上限 384 Mi 像素·帧的夹具在单测里编码太慢。）
    #[test]
    fn decode_volume_is_budgeted() {
        let (w, h) = (64u32, 32u32);
        let frames: Vec<([u8; 4], u32)> = (0..10).map(|i| ([i * 20, 0, 0, 255], 100)).collect();
        let gif = anim_fixtures::gif(w, h, &frames);
        let px = u64::from(w * h);
        // 预算 5 帧：解到第 6 帧前停，已解的 5 帧照报。
        let info = inspect_within(&gif, px * 5).unwrap();
        assert!(
            info.over_budget && info.truncated && info.animated,
            "{info:?}"
        );
        assert_eq!((info.width, info.height), (w, h));
        assert_eq!(info.frames.len(), 5, "保留超之前解出的帧");
        // 解下一帧前就判（解了才知道有没有下一帧）：恰好用满预算的文件也报超，故多留一帧。
        let full = inspect_within(&gif, px * 11).unwrap();
        assert!(!full.over_budget && !full.truncated && full.frames.len() == 10);
        let o = |from, to| ThumbOpts {
            max_edge: 8,
            from,
            to: Some(to),
        };
        let e = decode_within(&gif, &o(6, 8), px * 5).unwrap_err();
        assert!(e.contains("超出预算"), "{e}");
        let head = decode_within(&gif, &o(2, 5), px * 5).unwrap();
        assert_eq!(
            head.iter().map(|t| t.index).collect::<Vec<_>>(),
            [2, 3, 4],
            "只解到 to"
        );
        // 只够一帧：第 0 帧照报，按文件头当动画。
        let tiny = inspect_within(&gif, px).unwrap();
        assert!(tiny.over_budget && tiny.animated && tiny.frames.len() == 1);
    }

    /// 恰好 300 帧、预算恰好 300 帧：探测有没有第 301 帧不计预算，不误报截断或超预算。
    #[test]
    fn exactly_max_frames_is_not_flagged() {
        let max = wind_scene::image_cache::MAX_ANIM_FRAMES;
        let px = 4;
        let gif = |n: usize| {
            let frames: Vec<([u8; 4], u32)> = (0..n)
                .map(|i| ([(i % 256) as u8, 0, 0, 255], 100))
                .collect();
            anim_fixtures::gif(2, 2, &frames)
        };
        let info = inspect_within(&gif(max), px * max as u64).unwrap();
        assert!(
            !info.truncated && !info.over_budget,
            "{:?}",
            (info.truncated, info.over_budget)
        );
        assert_eq!(info.frames.len(), max);
        let more = inspect_within(&gif(max + 1), px * max as u64).unwrap();
        assert!(more.truncated && !more.over_budget);
        assert_eq!(more.frames.len(), max);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(inspect_image(b"not an image").is_err());
        assert!(inspect_image(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(inspect_image(&vec![0u8; MAX_IMAGE_BYTES + 1]).is_err());
        // 9000 宽：超出引擎位图上限，不解码。
        let e = inspect_image(&png(9000, 1, [0; 4])).unwrap_err();
        assert!(e.contains("超出上限"), "{e}");
        let ok = png(4, 4, [0; 4]);
        let d = ThumbOpts::default;
        for o in [
            ThumbOpts { max_edge: 0, ..d() },
            ThumbOpts {
                max_edge: MAX_THUMB_EDGE + 1,
                ..d()
            },
            ThumbOpts {
                from: 5,
                to: Some(4),
                ..d()
            },
            ThumbOpts {
                from: 0,
                to: Some(MAX_THUMB_FRAMES + 1),
                ..d()
            },
        ] {
            assert!(decode_frames(&ok, &o).is_err(), "{o:?}");
        }
    }

    #[test]
    fn thumbnails_are_downscaled_and_paged() {
        let colors: Vec<[u8; 4]> = (0..6).map(|i| [i * 40, 100, 0, 255]).collect();
        let gif = anim_fixtures::gif(
            40,
            20,
            &colors.iter().map(|&c| (c, 100)).collect::<Vec<_>>(),
        );
        let t = decode_frames(
            &gif,
            &ThumbOpts {
                max_edge: 10,
                from: 2,
                to: Some(4),
            },
        )
        .unwrap();
        assert_eq!(t.iter().map(|f| f.index).collect::<Vec<_>>(), [2, 3]);
        for f in &t {
            assert_eq!((f.w, f.h, f.rgba.len(), f.ms), (10, 5, 10 * 5 * 4, 100));
        }
        // GIF 调色板量化：颜色近似。
        assert!(t[0].rgba[0].abs_diff(80) <= 8, "{:?}", &t[0].rgba[..4]);
        // 越过末帧：只回有的；原图比上限小不放大。
        let tail = decode_frames(
            &gif,
            &ThumbOpts {
                max_edge: 256,
                from: 5,
                to: Some(9),
            },
        )
        .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!((tail[0].w, tail[0].h), (40, 20));
        // 半透明像素缩完是直通 alpha。
        let half = png(4, 4, [200, 100, 50, 128]);
        let t = decode_frames(
            &half,
            &ThumbOpts {
                max_edge: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let p = &t[0].rgba[..4];
        assert!(p[0].abs_diff(200) <= 2 && p[3].abs_diff(128) <= 1, "{p:?}");
    }
}
