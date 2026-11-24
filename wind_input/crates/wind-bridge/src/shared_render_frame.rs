//! SHM 帧编码（平台无关纯逻辑）：64B SharedRenderHeader + BGRA 像素 + hit-rect 表。
//! POSIX（macOS）与 Windows 写端共用；rect 表紧跟像素（C++ HostWindow.cpp:193-202 校验
//! rectsOffset >= 64 且表尾 <= maxBufferSize）。
use wind_ipc::protocol::{HostRenderHitRect, SharedRenderHeader};

pub struct FrameParams<'a> {
    pub sequence: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
    pub rects: &'a [HostRenderHitRect],
    pub rendered_hover_index: i32,
    pub target_instance_id: u32,
    pub software_shadow: bool,
    /// 穿透装饰层（与 `bgra` 同尺寸）+ 是否排在主层下面。追加在矩形表之后（设计 §17.8）。
    pub pass: Option<(&'a [u8], bool)>,
}

#[derive(Debug)]
pub struct FrameTooLarge;

pub fn encode_frame_into(dst: &mut [u8], p: &FrameParams) -> Result<(), FrameTooLarge> {
    let data_size = p.bgra.len();
    let rects_bytes = p.rects.len() * HostRenderHitRect::SIZE;
    // 穿透层与主层同尺寸；长度对不上（调用方错了）就不带它，不写半张。
    let pass = p.pass.filter(|(px, _)| px.len() == data_size);
    let pass_bytes = pass.map_or(0, |(px, _)| px.len());
    let total = SharedRenderHeader::SIZE + data_size + rects_bytes + pass_bytes;
    if total > dst.len() {
        return Err(FrameTooLarge);
    }
    let mut hdr =
        SharedRenderHeader::new(p.x, p.y, p.width, p.height, p.width * 4, data_size as u32);
    hdr.sequence = p.sequence;
    if p.software_shadow {
        hdr.flags |= SharedRenderHeader::FLAG_SOFTWARE_SHADOW;
    }
    hdr.rect_count = p.rects.len() as u32;
    hdr.rects_offset = if p.rects.is_empty() {
        0
    } else {
        (SharedRenderHeader::SIZE + data_size) as u32
    };
    hdr.rendered_hover_index = p.rendered_hover_index;
    hdr.target_instance_id = p.target_instance_id;
    // 矩形每条 20 字节、像素 4 字节对齐，穿透层起点天然 4 字节对齐。
    let pass_off = SharedRenderHeader::SIZE + data_size + rects_bytes;
    if let Some((_, below)) = pass {
        hdr.flags |= SharedRenderHeader::FLAG_PASS_LAYER;
        if below {
            hdr.flags |= SharedRenderHeader::FLAG_PASS_BELOW;
        }
        hdr.pass_offset = pass_off as u32;
    }
    // 先写像素与矩形、最后写 header：读端以 header.sequence 判新帧，
    // 避免读到「新头旧像素」的撕裂窗口（单写者，与 Go WriteFrame 顺序一致）。
    dst[SharedRenderHeader::SIZE..SharedRenderHeader::SIZE + data_size].copy_from_slice(p.bgra);
    let mut off = SharedRenderHeader::SIZE + data_size;
    for r in p.rects {
        dst[off..off + HostRenderHitRect::SIZE].copy_from_slice(&r.to_bytes());
        off += HostRenderHitRect::SIZE;
    }
    if let Some((px, _)) = pass {
        dst[pass_off..pass_off + px.len()].copy_from_slice(px);
    }
    dst[..SharedRenderHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    Ok(())
}

pub fn encode_hidden_into(dst: &mut [u8], sequence: u32, target_instance_id: u32) {
    assert!(
        dst.len() >= SharedRenderHeader::SIZE,
        "encode_hidden_into: dst 小于 SharedRenderHeader::SIZE"
    );
    let mut hdr = SharedRenderHeader::new(0, 0, 0, 0, 0, 0);
    hdr.flags = 0;
    hdr.sequence = sequence;
    hdr.target_instance_id = target_instance_id;
    dst[..SharedRenderHeader::SIZE].copy_from_slice(&hdr.to_bytes());
}

#[cfg(test)]
#[path = "shared_render_frame_tests.rs"]
mod tests;
