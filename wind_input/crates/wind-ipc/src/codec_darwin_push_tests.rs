use super::*;

fn cmd_of(frame: &[u8]) -> u16 {
    u16::from_le_bytes([frame[2], frame[3]])
}

#[test]
fn host_render_frame_layout_is_60_bytes_le() {
    let f = encode_host_render_frame(
        7,
        -3,
        20,
        100,
        40,
        0x3,
        2,
        (12, -5),
        (-3, -70),
        (12, 6, 76, 30),
    );
    assert_eq!(f.len(), 8 + 60);
    assert_eq!(cmd_of(&f), CMD_HOST_RENDER_FRAME);
    let p = &f[8..];
    assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 7);
    assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), -3);
    assert_eq!(i32::from_le_bytes(p[8..12].try_into().unwrap()), 20);
    assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 100);
    assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 40);
    assert_eq!(u32::from_le_bytes(p[20..24].try_into().unwrap()), 0x3);
    assert_eq!(u32::from_le_bytes(p[24..28].try_into().unwrap()), 2);
    assert_eq!(i32::from_le_bytes(p[28..32].try_into().unwrap()), 12);
    assert_eq!(i32::from_le_bytes(p[32..36].try_into().unwrap()), -5);
    let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    assert_eq!((i(36), i(40)), (-3, -70));
    assert_eq!((i(44), i(48), i(52), i(56)), (12, 6, 76, 30));
}

#[test]
fn overlay_frame_layout_is_72_bytes_le() {
    let f = encode_overlay_frame(&OverlayFrameMeta {
        kind: 2,
        seq: 9,
        width: 120,
        height: 40,
        flags: 0x5,
        place: 1,
        x: -30,
        y: 200,
        alt_x: -31,
        alt_y: 150,
        anchor: 7,
        margin: 16,
        content_x: 3,
        content_y: 4,
        content_w: 110,
        content_h: 30,
        duration_ms: 1500,
        scale: 2.0,
        drag: vec![],
    });
    assert_eq!(f.len(), 8 + 72);
    assert_eq!(cmd_of(&f), CMD_OVERLAY_FRAME);
    let p = &f[8..];
    let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    assert_eq!(
        (u(0), u(4), u(8), u(12), u(16), u(20)),
        (2, 9, 120, 40, 0x5, 1)
    );
    assert_eq!((i(24), i(28), i(32), i(36)), (-30, 200, -31, 150));
    assert_eq!(u(40), 7);
    assert_eq!((i(44), i(48), i(52)), (16, 3, 4));
    assert_eq!((u(56), u(60)), (110, 30));
    assert_eq!(i(64), 1500);
    assert_eq!(f32::from_le_bytes(p[68..72].try_into().unwrap()), 2.0);
}

#[test]
fn candidate_skin_layout_matches_documented_offsets() {
    use crate::protocol::skin::*;
    let f = encode_candidate_skin(&CandidateSkinWire {
        flags: SKIN_FLAG_ACTIVE | SKIN_FLAG_MASK,
        frame: [10, 20, 300, 120],
        decor: vec![
            SkinDecorWire {
                index: 2,
                hit: SKIN_HIT_PASS,
                flags: SKIN_DECOR_UNDER | SKIN_DECOR_SPLIT,
                rect: [-5, -6, 40, 50],
            },
            SkinDecorWire {
                index: 0,
                hit: SKIN_HIT_BLOCK,
                flags: 0,
                rect: [1, 2, 3, 4],
            },
        ],
        mask: vec![[10, 20, 300, 1], [0, 0, 7, 8]],
    });
    assert_eq!(cmd_of(&f), CMD_CANDIDATE_SKIN);
    let p = &f[IpcHeader::SIZE..];
    assert_eq!(p.len(), 4 + 16 + 4 + 2 * 28 + 4 + 2 * 16);
    let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    assert_eq!(u(0), 3);
    assert_eq!((i(4), i(8), i(12), i(16)), (10, 20, 300, 120));
    assert_eq!(u(20), 2);
    assert_eq!((u(24), u(28), u(32)), (2, 2, 3));
    assert_eq!((i(36), i(40), i(44), i(48)), (-5, -6, 40, 50));
    assert_eq!((u(52), u(56), u(60)), (0, 1, 0));
    assert_eq!((i(64), i(68), i(72), i(76)), (1, 2, 3, 4));
    assert_eq!(u(80), 2);
    assert_eq!((i(84), i(88), i(92), i(96)), (10, 20, 300, 1));
    assert_eq!((i(100), i(104), i(108), i(112)), (0, 0, 7, 8));
    // 没启用皮肤：28 字节全零。
    let none = encode_candidate_skin(&CandidateSkinWire::default());
    assert_eq!(&none[IpcHeader::SIZE..], &[0u8; 28][..]);
}

#[test]
fn overlay_frame_drag_tail_only_for_panel_kinds() {
    let base = OverlayFrameMeta {
        kind: 3,
        drag: vec![[1, 2, 3, 4], [5, 6, 7, 8]],
        ..Default::default()
    };
    // 非交互层：带不带拖动区都是 72 字节，逐字节相同。
    let plain = encode_overlay_frame(&base);
    assert_eq!(plain.len(), 8 + 72);
    let no_drag = encode_overlay_frame(&OverlayFrameMeta {
        drag: vec![],
        ..base.clone()
    });
    assert_eq!(plain, no_drag);

    // 工具栏：多 4 + 2*16 字节，并能原样解回。
    let f = encode_overlay_frame(&OverlayFrameMeta {
        kind: OVERLAY_KIND_TOOLBAR,
        ..base.clone()
    });
    assert_eq!(f.len(), 8 + 72 + 4 + 2 * 16);
    let p = &f[8 + 72..];
    let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    assert_eq!(i(0), 2);
    let rects: Vec<[i32; 4]> = (0..2)
        .map(|k| std::array::from_fn(|j| i(4 + k * 16 + j * 4)))
        .collect();
    assert_eq!(rects, base.drag);

    // 软键盘同样带尾段；零个拖动区只多 count。
    let f = encode_overlay_frame(&OverlayFrameMeta {
        kind: OVERLAY_KIND_SOFTKEYBOARD,
        drag: vec![],
        ..base
    });
    assert_eq!(f.len(), 8 + 72 + 4);
}

#[test]
fn panel_pointer_roundtrip() {
    let p = encode_panel_pointer(11, 3, 1, -5, 40);
    assert_eq!(p.len(), 20);
    assert_eq!(decode_panel_pointer(&p), Some((11, 3, 1, -5, 40)));
    assert_eq!(decode_panel_pointer(&p[..19]), None);
}

#[test]
fn candidate_rects_layout_count_then_5xi32() {
    let f = encode_candidate_rects(&[(0, 1, 2, 30, 24), (-1, 5, 6, 12, 12)]);
    assert_eq!(cmd_of(&f), CMD_CANDIDATE_RECTS);
    let p = &f[8..];
    assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 2);
    assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), 0);
    assert_eq!(i32::from_le_bytes(p[8..12].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(p[24..28].try_into().unwrap()), -1);
}

#[test]
fn mode_status_label_utf8_length_prefixed() {
    let f = encode_mode_status(0x5, 1, "五笔");
    assert_eq!(cmd_of(&f), CMD_MODE_STATUS);
    let p = &f[8..];
    assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 0x5);
    assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 1);
    let n = u32::from_le_bytes(p[8..12].try_into().unwrap()) as usize;
    assert_eq!(n, "五笔".len());
    assert_eq!(&p[12..12 + n], "五笔".as_bytes());
}

// 递归解码器：镜像 Swift BinaryCodec.decodeMenuItems，作为 encode_menu_show 的规范验证。
struct DecodedItem {
    id: i32,
    flags: u8,
    label: String,
    children: Vec<DecodedItem>,
}
fn decode_menu_items(p: &[u8], off: &mut usize) -> Vec<DecodedItem> {
    let n = u32::from_le_bytes(p[*off..*off + 4].try_into().unwrap()) as usize;
    *off += 4;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let id = i32::from_le_bytes(p[*off..*off + 4].try_into().unwrap());
        *off += 4;
        let flags = p[*off];
        *off += 1;
        let ln = u32::from_le_bytes(p[*off..*off + 4].try_into().unwrap()) as usize;
        *off += 4;
        let label = String::from_utf8(p[*off..*off + ln].to_vec()).unwrap();
        *off += ln;
        let children = decode_menu_items(p, off);
        out.push(DecodedItem {
            id,
            flags,
            label,
            children,
        });
    }
    out
}

#[test]
fn menu_show_roundtrips_nested_tree_le() {
    let tree = vec![
        MenuNode {
            id: 100,
            label: "英文".into(),
            checked: true,
            ..Default::default()
        },
        MenuNode {
            id: 0,
            label: "主题".into(),
            children: vec![
                MenuNode {
                    id: 2000,
                    label: "默认".into(),
                    checked: true,
                    ..Default::default()
                },
                MenuNode {
                    separator: true,
                    ..Default::default()
                },
                MenuNode {
                    id: 4001,
                    label: "亮色".into(),
                    disabled: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    ];
    let f = encode_menu_show(&tree);
    assert_eq!(cmd_of(&f), CMD_MENU_SHOW);
    let p = &f[8..];
    let mut off = 0usize;
    let top = decode_menu_items(p, &mut off);
    assert_eq!(off, p.len(), "整帧应被消费完（无游离字节）");
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].id, 100);
    assert_eq!(top[0].flags & 0x02, 0x02); // checked
    assert_eq!(top[0].label, "英文");
    assert!(top[0].children.is_empty());
    // 子菜单父项 id=0、无勾选，含 3 子项
    assert_eq!(top[1].label, "主题");
    assert_eq!(top[1].id, 0);
    let sub = &top[1].children;
    assert_eq!(sub.len(), 3);
    assert_eq!(sub[0].id, 2000);
    assert_eq!(sub[0].flags & 0x02, 0x02); // checked
    assert_eq!(sub[1].flags & 0x01, 0x01); // separator
    assert_eq!(sub[2].id, 4001);
    assert_eq!(sub[2].flags & 0x04, 0x04); // disabled
}
