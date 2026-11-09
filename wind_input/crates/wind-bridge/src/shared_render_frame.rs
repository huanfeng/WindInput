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
mod tests {
    use super::*;
    use wind_ipc::protocol::{HostRenderHitRect, SharedRenderHeader};

    #[test]
    fn frame_layout_header_pixels_rects() {
        let bgra = vec![0xAAu8; 2 * 2 * 4];
        let rects = [HostRenderHitRect {
            index: 0,
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }];
        let mut dst = vec![0u8; 4096];
        encode_frame_into(
            &mut dst,
            &FrameParams {
                sequence: 5,
                x: 10,
                y: 20,
                width: 2,
                height: 2,
                bgra: &bgra,
                rects: &rects,
                rendered_hover_index: -1,
                target_instance_id: 7,
                software_shadow: false,
                pass: None,
            },
        )
        .unwrap();
        let hdr: SharedRenderHeader = unsafe { std::ptr::read_unaligned(dst.as_ptr() as *const _) };
        assert_eq!({ hdr.magic }, 0x57494E44);
        assert_eq!({ hdr.sequence }, 5);
        assert_eq!({ hdr.target_instance_id }, 7);
        assert_eq!({ hdr.rect_count }, 1);
        // rect 表紧跟像素：offset = 64 + data_size
        assert_eq!({ hdr.rects_offset }, 64 + 16);
        assert_eq!({ hdr.data_size }, 16);
        assert_ne!({ hdr.flags } & SharedRenderHeader::FLAG_VISIBLE, 0);
        // 像素与矩形字节
        assert_eq!(&dst[64..80], &bgra[..]);
        assert_eq!(&dst[80..100], &rects[0].to_bytes());
    }

    /// 穿透层追加在矩形表之后，头里带偏移与上 / 下位；没带时偏移为 0、两位都不置。
    #[test]
    fn pass_layer_follows_rects() {
        let bgra = vec![0x11u8; 2 * 2 * 4];
        let pass = vec![0x22u8; 2 * 2 * 4];
        let rects = [HostRenderHitRect {
            index: 0,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        }];
        let params = |pass: Option<(&'static [u8], bool)>| FrameParams {
            sequence: 1,
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            bgra: Box::leak(bgra.clone().into_boxed_slice()),
            rects: Box::leak(Box::new(rects)),
            rendered_hover_index: -1,
            target_instance_id: 1,
            software_shadow: false,
            pass,
        };
        let pass: &'static [u8] = Box::leak(pass.into_boxed_slice());
        let read = |dst: &[u8]| -> SharedRenderHeader {
            unsafe { std::ptr::read_unaligned(dst.as_ptr() as *const _) }
        };
        for below in [false, true] {
            let mut dst = vec![0u8; 4096];
            encode_frame_into(&mut dst, &params(Some((pass, below)))).unwrap();
            let hdr = read(&dst);
            let off = 64 + 16 + 20;
            assert_eq!({ hdr.pass_offset }, off as u32);
            assert_ne!({ hdr.flags } & SharedRenderHeader::FLAG_PASS_LAYER, 0);
            assert_eq!(
                { hdr.flags } & SharedRenderHeader::FLAG_PASS_BELOW != 0,
                below
            );
            assert_eq!(&dst[off..off + 16], pass);
            assert_eq!(&dst[64..80], &bgra[..], "主层不被穿透层覆盖");
        }
        let mut dst = vec![0u8; 4096];
        encode_frame_into(&mut dst, &params(None)).unwrap();
        let hdr = read(&dst);
        assert_eq!({ hdr.pass_offset }, 0);
        assert_eq!(
            { hdr.flags }
                & (SharedRenderHeader::FLAG_PASS_LAYER | SharedRenderHeader::FLAG_PASS_BELOW),
            0
        );
        // 尺寸对不上的穿透层不写；放不下整帧（含穿透层）报过大。
        let short: &'static [u8] = Box::leak(vec![0u8; 8].into_boxed_slice());
        encode_frame_into(&mut dst, &params(Some((short, false)))).unwrap();
        assert_eq!({ read(&dst).pass_offset }, 0);
        let mut small = vec![0u8; 64 + 16 + 20 + 15];
        assert!(encode_frame_into(&mut small, &params(Some((pass, false)))).is_err());
        assert!(encode_frame_into(&mut small, &params(None)).is_ok());
    }

    #[test]
    fn hidden_frame_broadcast_target_zero() {
        let mut dst = vec![0u8; 128];
        encode_hidden_into(&mut dst, 9, 0);
        let hdr: SharedRenderHeader = unsafe { std::ptr::read_unaligned(dst.as_ptr() as *const _) };
        assert_eq!({ hdr.flags } & SharedRenderHeader::FLAG_VISIBLE, 0);
        assert_eq!({ hdr.sequence }, 9);
        assert_eq!({ hdr.target_instance_id }, 0);
        assert_eq!({ hdr.magic }, 0x57494E44); // 隐藏帧仍是合法帧（C++ 校验魔数）
    }

    #[test]
    fn frame_too_large_rejected() {
        let bgra = vec![0u8; 256];
        let mut dst = vec![0u8; 128]; // 64 头 + 64 容量 < 256 像素
        assert!(
            encode_frame_into(
                &mut dst,
                &FrameParams {
                    sequence: 1,
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                    bgra: &bgra,
                    rects: &[],
                    rendered_hover_index: -1,
                    target_instance_id: 1,
                    software_shadow: false,
                    pass: None,
                }
            )
            .is_err()
        );
    }

    #[test]
    #[should_panic(expected = "encode_hidden_into")]
    fn hidden_into_undersized_dst_panics_with_contract_message() {
        let mut dst = vec![0u8; 32];
        encode_hidden_into(&mut dst, 1, 0);
    }
}
