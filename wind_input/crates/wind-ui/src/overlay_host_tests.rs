use super::*;

/// macOS 宿主坐标是点、位图是 scale 倍像素：像素长度折成点，反向取整互逆；Linux 恒不换算。
#[test]
fn host_units_follow_platform() {
    assert_eq!(px_per_unit_for(2.0, true), 2.0);
    assert_eq!(px_per_unit_for(0.5, true), 1.0, "缩放不会小于 1");
    assert_eq!(px_per_unit_for(2.0, false), 1.0);
    let k = px_per_unit_for(2.0, true);
    assert_eq!((61.0_f32 / k).round() as i32, 31);
    assert_eq!((30.0_f32 * k).round() as i32, 60);
}

#[test]
fn caret_place_below_with_offset_and_flip_above_without() {
    // 光标底 120、高 20；偏移 (5, 7)；内容高 30；gap 4。
    let p = status_caret_place(100, 120, 20, (5, 7), 30, 4, false);
    assert_eq!(
        p,
        Place::Flip {
            x: 105,
            y: 131,
            alt_x: 105,
            alt_y: 120 - 20 - 30 - 4
        }
    );
}

#[test]
fn caret_place_above_puts_offset_on_top_and_flips_below() {
    let p = status_caret_place(100, 120, 20, (5, 7), 30, 4, true);
    assert_eq!(
        p,
        Place::Flip {
            x: 105,
            y: 120 - 20 - 30 - 4 + 7,
            alt_x: 105,
            alt_y: 124
        }
    );
}

#[test]
fn fixed_unset_sentinel_falls_back_to_caret() {
    assert_eq!(
        status_fixed_place(0, 0, 300, 400),
        Place::Absolute { x: 300, y: 400 }
    );
    assert_eq!(
        status_fixed_place(-800, 10, 300, 400),
        Place::Absolute { x: -800, y: 10 },
        "负坐标（左侧副屏）是合法落点，不是哨兵"
    );
}

#[test]
fn window_anchors_degrade_to_screen_anchors() {
    assert_eq!(
        status_anchor_code(StatusTipAnchor::WindowCenter),
        status_anchor_code(StatusTipAnchor::ScreenCenter)
    );
    assert_eq!(
        status_anchor_code(StatusTipAnchor::WindowBottomLeft),
        OVERLAY_ANCHOR_BOTTOM_LEFT
    );
    assert_eq!(
        status_anchor_code(StatusTipAnchor::ScreenTopRight),
        OVERLAY_ANCHOR_TOP_RIGHT
    );
}

#[test]
fn toast_positions_map_to_distinct_anchors() {
    let all = [
        ToastPosition::Center,
        ToastPosition::TopCenter,
        ToastPosition::BottomCenter,
        ToastPosition::TopLeft,
        ToastPosition::TopRight,
        ToastPosition::BottomLeft,
        ToastPosition::BottomRight,
    ];
    let mut codes: Vec<u32> = all.iter().map(|p| toast_anchor_code(*p)).collect();
    codes.sort();
    codes.dedup();
    assert_eq!(codes.len(), all.len());
}

#[test]
fn tooltip_below_row_flips_above_row() {
    let p = tooltip_place((50, 100, 150, 130), (80, 24), false);
    assert_eq!(
        p,
        Place::FollowCandidate {
            x: 50,
            y: 132,
            alt_x: 50,
            alt_y: 100 - 2 - 24
        }
    );
}

#[test]
fn tooltip_beside_prefers_right_then_left() {
    let p = tooltip_place((50, 100, 150, 130), (80, 24), true);
    assert_eq!(
        p,
        Place::FollowCandidate {
            x: 154,
            y: 100,
            alt_x: 50 - 4 - 80,
            alt_y: 100
        }
    );
}

struct Cap(std::sync::Mutex<Vec<Vec<u8>>>);
impl HostRenderSink for Cap {
    fn push_frame(&self, f: &[u8]) {
        self.0.lock().unwrap().push(f.to_vec());
    }
}

#[test]
fn hide_without_show_sends_nothing_and_show_then_hide_sends_hidden_frame() {
    let cap = Cap(std::sync::Mutex::new(Vec::new()));
    // 名字带进程号，并发测试不撞段。
    let mut shms = OverlayShms::new(format!("_t{}", std::process::id()));
    shms.hide(&cap, OVERLAY_KIND_STATUS);
    assert!(cap.0.lock().unwrap().is_empty(), "没显示过就不该发隐藏帧");
    let ov = Overlay {
        buf: vec![255; 4 * 4 * 2],
        width: 4,
        height: 2,
        content_x: 0,
        content_y: 0,
        content_w: 4,
        content_h: 2,
        software_shadow: false,
        place: Place::Anchor {
            anchor: OVERLAY_ANCHOR_CENTER,
            margin: 16,
        },
        scale: 1.0,
    };
    shms.show(&cap, OVERLAY_KIND_STATUS, &ov, 1200);
    shms.hide(&cap, OVERLAY_KIND_STATUS);
    let frames = cap.0.lock().unwrap().clone();
    assert_eq!(frames.len(), 2);
    let u = |f: &[u8], o: usize| u32::from_le_bytes(f[8 + o..12 + o].try_into().unwrap());
    assert_eq!(u(&frames[0], 0), OVERLAY_KIND_STATUS);
    assert_ne!(u(&frames[0], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
    assert_eq!(u(&frames[0], 20), OVERLAY_PLACE_ANCHOR);
    assert_eq!(u(&frames[1], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
    assert!(u(&frames[1], 4) > u(&frames[0], 4), "隐藏帧也推进 seq");
}

/// 截图从 SHM 读回的就是推下去的那一帧；藏了就没有。
#[test]
fn snapshot_reads_back_shown_frame_only() {
    let cap = Cap(std::sync::Mutex::new(Vec::new()));
    let mut shms = OverlayShms::new(format!("_ts{}", std::process::id()));
    assert!(shms.snapshot(OVERLAY_KIND_TOOLTIP).is_none(), "没显示过");
    let buf: Vec<u8> = (0..3 * 2 * 4).map(|i| i as u8).collect();
    let ov = Overlay {
        buf: buf.clone(),
        width: 3,
        height: 2,
        content_x: 0,
        content_y: 0,
        content_w: 3,
        content_h: 2,
        software_shadow: false,
        place: Place::Absolute { x: 0, y: 0 },
        scale: 1.0,
    };
    shms.show(&cap, OVERLAY_KIND_TOOLTIP, &ov, 0);
    assert_eq!(shms.snapshot(OVERLAY_KIND_TOOLTIP), Some((buf, 3, 2)));
    assert!(shms.snapshot(OVERLAY_KIND_STATUS).is_none(), "各层各管各的");
    shms.hide(&cap, OVERLAY_KIND_TOOLTIP);
    assert!(shms.snapshot(OVERLAY_KIND_TOOLTIP).is_none(), "藏了就不给");
}
