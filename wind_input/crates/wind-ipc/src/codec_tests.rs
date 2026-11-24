use super::*;

/// 从三种状态编码里取出 flags 字段（三者布局相同：header 之后第一个 u32）。
fn flags_of(buf: &[u8]) -> u32 {
    // 偏移写错的话下面 `on ^ off` 那条断言会红：取到别处的字节不可能恰好只差
    // STATUS_HOTKEY_SESSION 这一位。
    u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]])
}

/// 「把会话位编码进一条状态消息」的三个入口的共同形状。
/// 抽成别名只为躲开 clippy::type_complexity；三个入口签名一致正是本用例的前提。
type StatusEncoder = Box<dyn Fn(bool) -> Vec<u8>>;

/// 三条状态通路都必须把 `hotkey_session` 编进 flags。
///
/// 这一位是 level-triggered 的：C++ 侧收到任何状态响应都会无条件镜像它，漏带会把正活着的
/// 会话当场清掉（如加词开着时 lshift 切中英，加词的 Enter/Esc 立刻失灵）。三个入口的参数
/// 全是相邻的 bool，漏传/错位编译器不报错，只能靠本条钉住。
#[test]
fn every_status_encoding_carries_the_hotkey_session_bit() {
    let cases: [(&str, StatusEncoder); 3] = [
        (
            "encode_status_update",
            Box::new(|on| {
                encode_status_update(
                    true,
                    false,
                    true,
                    true,
                    false,
                    false,
                    false,
                    false,
                    on,
                    &[],
                    &[],
                    "中",
                )
            }),
        ),
        (
            "encode_activation_status_push",
            Box::new(|on| {
                encode_activation_status_push(
                    true,
                    false,
                    true,
                    true,
                    false,
                    false,
                    false,
                    false,
                    on,
                    &[],
                    &[],
                    "中",
                )
            }),
        ),
        (
            "encode_state_push",
            Box::new(|on| {
                encode_state_push(true, false, true, true, false, false, false, on, "中")
            }),
        ),
    ];

    for (name, enc) in cases {
        let on = flags_of(&enc(true));
        let off = flags_of(&enc(false));
        assert_ne!(
            on & STATUS_HOTKEY_SESSION,
            0,
            "{name}: hotkey_session=true 时必须置 STATUS_HOTKEY_SESSION（flags=0x{on:08X}）"
        );
        assert_eq!(
            off & STATUS_HOTKEY_SESSION,
            0,
            "{name}: hotkey_session=false 时不得置位（flags=0x{off:08X}）"
        );
        // 只有这一位变——防止参数错位把别的状态位一起改了。
        assert_eq!(
            on ^ off,
            STATUS_HOTKEY_SESSION,
            "{name}: 切换 hotkey_session 只应改变这一位，实际差异=0x{:08X}",
            on ^ off
        );
    }
}

/// 拼一份 macOS `.app` 形态的 FOCUS_GAINED 载荷（39 定长 + bundleID 段）。
fn focus_payload_with_bundle(id: &str) -> Vec<u8> {
    let mut p = vec![0u8; 39];
    p.extend_from_slice(&(id.len() as u32).to_le_bytes());
    p.extend_from_slice(id.as_bytes());
    p
}

#[test]
fn uielement_state_and_action_roundtrip() {
    let st = UiElementStatePayload {
        pid: 4242,
        flags: UIELEMENT_FLAG_HOST_DRAWS,
    };
    assert_eq!(decode_uielement_state(&st.to_bytes()).unwrap(), st);
    assert!(st.host_draws());
    // UI-less 线程位单独置位也算接管（从激活起就不弹窗，见常量注释）。
    assert!(
        UiElementStatePayload {
            pid: 1,
            flags: UIELEMENT_FLAG_UI_LESS_THREAD
        }
        .host_draws()
    );
    assert!(!UiElementStatePayload { pid: 1, flags: 0 }.host_draws());
    assert!(decode_uielement_state(&[0u8; 7]).is_err());

    let act = UiElementActionPayload {
        action: UIELEMENT_ACTION_SET_SELECTION,
        arg: 17,
    };
    assert_eq!(decode_uielement_action(&act.to_bytes()).unwrap(), act);
    assert!(decode_uielement_action(&[0u8; 3]).is_err());
}

#[test]
fn uielement_page_roundtrip() {
    let page = UiElementPage {
        items: vec!["你好".into(), "拟好".into(), "".into(), "abc".into()],
        selected: 1,
        page_size: 9,
        current_page: 0,
    };
    let frame = encode_uielement_page(&page);
    assert_eq!(u16::from_le_bytes([frame[2], frame[3]]), CMD_UIELEMENT_PAGE);
    let body_len = u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize;
    assert_eq!(body_len, frame.len() - 8);
    assert_eq!(decode_uielement_page(&frame[8..]).unwrap(), page);
    // 空列表也要能往返（宿主 Abort 后 DLL 可能在 EndUIElement 前再拉一次）。
    let empty = UiElementPage {
        page_size: 5,
        ..Default::default()
    };
    assert_eq!(
        decode_uielement_page(&encode_uielement_page(&empty)[8..]).unwrap(),
        empty
    );
    // page_size=0 会让 DLL 侧除零：编码器钉到 ≥1。
    let zero = UiElementPage {
        items: vec!["a".into()],
        page_size: 0,
        ..Default::default()
    };
    assert_eq!(
        decode_uielement_page(&encode_uielement_page(&zero)[8..])
            .unwrap()
            .page_size,
        1
    );
    // 条数超上限按 UIELEMENT_MAX_ITEMS 截断，count 与实际条数一致。
    let long = UiElementPage {
        items: (0..UIELEMENT_MAX_ITEMS + 50)
            .map(|i| i.to_string())
            .collect(),
        page_size: 9,
        ..Default::default()
    };
    let decoded = decode_uielement_page(&encode_uielement_page(&long)[8..]).unwrap();
    assert_eq!(decoded.items.len(), UIELEMENT_MAX_ITEMS);
    // 截断的载荷必须报错而不是静默给半截列表。
    let mut truncated = encode_uielement_page(&page)[8..].to_vec();
    truncated.truncate(truncated.len() - 2);
    assert!(decode_uielement_page(&truncated).is_err());
}

#[test]
fn ext_envelope_roundtrip() {
    let f = encode_ext("settings.open", br#"{"args":["--page=dict"]}"#);
    assert_eq!(u16::from_le_bytes([f[2], f[3]]), CMD_EXT);
    let (kind, body) = decode_ext(&f[8..]).expect("解不出信封");
    assert_eq!(kind, "settings.open");
    assert_eq!(body, br#"{"args":["--page=dict"]}"#);
    // 空 body（纯信号型 kind）也须能往返。
    let f2 = encode_ext("diag.hud", b"");
    assert_eq!(decode_ext(&f2[8..]), Some(("diag.hud", &b""[..])));
}

#[test]
fn ext_envelope_rejects_truncated_or_invalid() {
    // 截断：kind 长度声明超出实际字节。
    let mut bad = 99u32.to_le_bytes().to_vec();
    bad.extend_from_slice(b"abc");
    assert_eq!(decode_ext(&bad), None);
    // 截断：body 长度声明超出实际字节。
    let mut bad2 = 3u32.to_le_bytes().to_vec();
    bad2.extend_from_slice(b"abc");
    bad2.extend_from_slice(&99u32.to_le_bytes());
    assert_eq!(decode_ext(&bad2), None);
    // 非法 UTF-8 的 kind。
    let mut bad3 = 2u32.to_le_bytes().to_vec();
    bad3.extend_from_slice(&[0xFF, 0xFE]);
    bad3.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(decode_ext(&bad3), None);
    // 空载荷。
    assert_eq!(decode_ext(&[]), None);
}

#[test]
fn focus_gained_bundle_id_roundtrip() {
    let p = focus_payload_with_bundle("com.apple.TextEdit");
    assert_eq!(decode_focus_gained_bundle_id(&p), "com.apple.TextEdit");
    // 定长段本身仍须能解（bundleID 是纯追加，不影响既有字段）。
    assert!(decode_focus_gained(&p).is_ok());
}

#[test]
fn focus_gained_bundle_id_absent_or_malformed_is_empty() {
    // Windows DLL 的 39 字节包：无 bundleID 段。
    assert_eq!(decode_focus_gained_bundle_id(&[0u8; 39]), "");
    // 旧 macOS `.app` 的 12 字节短包。
    assert_eq!(decode_focus_gained_bundle_id(&[0u8; 12]), "");
    // 长度字段越界（截断的帧）：不得 panic，按「取不到」处理。
    let mut p = vec![0u8; 39];
    p.extend_from_slice(&999u32.to_le_bytes());
    p.extend_from_slice(b"abc");
    assert_eq!(decode_focus_gained_bundle_id(&p), "");
    // 非法 UTF-8 同样按「取不到」处理。
    let mut q = vec![0u8; 39];
    q.extend_from_slice(&2u32.to_le_bytes());
    q.extend_from_slice(&[0xFF, 0xFE]);
    assert_eq!(decode_focus_gained_bundle_id(&q), "");
}

/// 两个变长段的组合：`[39][bundleIdLen][bundleId][classLen][class]`。
fn focus_payload_with_sections(bundle: &str, class: &str) -> Vec<u8> {
    let mut p = vec![0u8; 39];
    p.extend_from_slice(&(bundle.len() as u32).to_le_bytes());
    p.extend_from_slice(bundle.as_bytes());
    p.extend_from_slice(&(class.len() as u32).to_le_bytes());
    p.extend_from_slice(class.as_bytes());
    p
}

#[test]
fn focus_gained_window_class_roundtrip_on_both_platform_shapes() {
    // Windows 形态：bundleId 空占位 + 类名。
    let win = focus_payload_with_sections("", "Shell_TrayWnd");
    assert_eq!(decode_focus_gained_window_class(&win), "Shell_TrayWnd");
    assert_eq!(decode_focus_gained_bundle_id(&win), "");

    // macOS 形态：两段都非空。钉住类名段必须顺序走过 bundleId：改成固定偏移时非空 bundleId
    // 会让它读到垃圾，而定长字段全对、别的测试没有信号。
    let mac = focus_payload_with_sections("com.apple.TextEdit", "NSWindow");
    assert_eq!(decode_focus_gained_bundle_id(&mac), "com.apple.TextEdit");
    assert_eq!(decode_focus_gained_window_class(&mac), "NSWindow");

    // 定长段不受影响
    assert!(decode_focus_gained(&win).is_ok());
    assert!(decode_focus_gained(&mac).is_ok());
}

#[test]
fn focus_gained_window_class_absent_or_malformed_is_empty() {
    // 旧 DLL 的 39 字节包：两段都没有。
    assert_eq!(decode_focus_gained_window_class(&[0u8; 39]), "");
    // 只有 bundleId 段（本次改动之前的 macOS 包）。
    assert_eq!(
        decode_focus_gained_window_class(&focus_payload_with_bundle("com.apple.TextEdit")),
        ""
    );
    // bundleId 段自身越界 ⇒ 后面不可能有东西，不得 panic。
    let mut p = vec![0u8; 39];
    p.extend_from_slice(&999u32.to_le_bytes());
    p.extend_from_slice(b"abc");
    assert_eq!(decode_focus_gained_window_class(&p), "");
    // 类名段长度越界。
    let mut q = focus_payload_with_sections("", "");
    q.truncate(q.len() - 4);
    q.extend_from_slice(&999u32.to_le_bytes());
    assert_eq!(decode_focus_gained_window_class(&q), "");
}

/// 三个变长段：`[39][bundleIdLen][bundleId][classLen][class][titleLen][title]`。
fn focus_payload_with_title(bundle: &str, class: &str, title: &str) -> Vec<u8> {
    let mut p = focus_payload_with_sections(bundle, class);
    p.extend_from_slice(&(title.len() as u32).to_le_bytes());
    p.extend_from_slice(title.as_bytes());
    p
}

#[test]
fn focus_gained_window_title_roundtrip_on_both_platform_shapes() {
    // Windows 形态（新 DLL、服务端推开了采集）：bundleId 空占位 + 类名 + 标题。
    let win = focus_payload_with_title("", "Chrome_WidgetWin_1", "新标签页 - Google Chrome");
    assert_eq!(
        decode_focus_gained_window_title(&win),
        "新标签页 - Google Chrome"
    );
    assert_eq!(decode_focus_gained_window_class(&win), "Chrome_WidgetWin_1");
    assert_eq!(decode_focus_gained_bundle_id(&win), "");
    // 非空 bundleId + 类名：标题段必须顺序走过前两段，固定偏移会读到垃圾。
    let mac = focus_payload_with_title("com.apple.TextEdit", "NSWindow", "Untitled");
    assert_eq!(decode_focus_gained_window_title(&mac), "Untitled");
    assert_eq!(decode_focus_gained_window_class(&mac), "NSWindow");
    // 新 DLL、未推开采集：标题段在、长度 0。
    let off = focus_payload_with_title("", "Notepad", "");
    assert_eq!(decode_focus_gained_window_title(&off), "");
    assert_eq!(decode_focus_gained_window_class(&off), "Notepad");
    // 定长段不受影响
    let fg = decode_focus_gained(&win).unwrap();
    assert_eq!(fg.client_token, 0);
}

/// 新旧互通：旧 DLL 的帧（没有标题段）解出空串，类名照旧；截断 / 越界 / 非法 UTF-8 不 panic。
#[test]
fn focus_gained_window_title_absent_or_malformed_is_empty() {
    // 旧 DLL：39 字节、或只带 bundleId + 类名两段（P3 之前的 Windows DLL）。
    assert_eq!(decode_focus_gained_window_title(&[0u8; 39]), "");
    let old = focus_payload_with_sections("", "Shell_TrayWnd");
    assert_eq!(decode_focus_gained_window_title(&old), "");
    assert_eq!(decode_focus_gained_window_class(&old), "Shell_TrayWnd");
    // 只有 bundleId 段（旧 macOS 包）。
    assert_eq!(
        decode_focus_gained_window_title(&focus_payload_with_bundle("com.apple.TextEdit")),
        ""
    );
    // 类名段越界 ⇒ 后面不可能有东西。
    let mut p = focus_payload_with_sections("", "");
    p.truncate(p.len() - 4);
    p.extend_from_slice(&999u32.to_le_bytes());
    assert_eq!(decode_focus_gained_window_title(&p), "");
    // 标题段长度越界。
    let mut q = focus_payload_with_title("", "C", "");
    q.truncate(q.len() - 4);
    q.extend_from_slice(&999u32.to_le_bytes());
    q.extend_from_slice(b"abc");
    assert_eq!(decode_focus_gained_window_title(&q), "");
    assert_eq!(
        decode_focus_gained_window_class(&q),
        "C",
        "标题段坏了不连累类名"
    );
    // 标题非法 UTF-8。
    let mut r = focus_payload_with_sections("", "C");
    r.extend_from_slice(&2u32.to_le_bytes());
    r.extend_from_slice(&[0xFF, 0xFE]);
    assert_eq!(decode_focus_gained_window_title(&r), "");
}

/// 长度域取极值（`u32::MAX`）：64 位上越界、32 位上 `start + n` 会回绕——一律按「取不到」。
#[test]
fn focus_gained_sections_with_huge_lengths_are_empty_not_wrapped() {
    let mut p = focus_payload_with_sections("", "C");
    p.extend_from_slice(&u32::MAX.to_le_bytes());
    p.extend_from_slice(b"abc");
    assert_eq!(decode_focus_gained_window_title(&p), "");
    let mut q = vec![0u8; 39];
    q.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(decode_focus_gained_window_class(&q), "");
    assert_eq!(
        read_len_prefixed(&q, usize::MAX - 2),
        ("", None),
        "偏移本身溢出"
    );
}

#[test]
fn compat_title_match_value_is_one_byte_flag() {
    assert_eq!(encode_compat_title_match_value(true), vec![1]);
    assert_eq!(encode_compat_title_match_value(false), vec![0]);
}

#[test]
fn test_encode_host_render_setup_layout_matches_cpp() {
    use crate::protocol::HostRenderSetupEntry;
    let entries = vec![HostRenderSetupEntry {
        window_kind: 0,
        max_buffer_size: 4 * 1024 * 1024,
        shm_name: "Local\\W_SHM".to_string(),   // 11 B
        event_name: "Local\\W_EVT".to_string(), // 11 B
    }];
    let buf = encode_host_render_setup(7, &entries);
    // IpcHeader 8B: version u16 + command u16 + payload_len u32
    let p = &buf[8..];
    // instanceId(4) + entryCount(4)
    assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 7);
    assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 1);
    // entry header 16B: kind + maxBufferSize + shmNameLen + eventNameLen
    assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), 0);
    assert_eq!(
        u32::from_le_bytes(p[12..16].try_into().unwrap()),
        4 * 1024 * 1024
    );
    assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 11);
    assert_eq!(u32::from_le_bytes(p[20..24].try_into().unwrap()), 11);
    assert_eq!(&p[24..35], b"Local\\W_SHM");
    assert_eq!(&p[35..46], b"Local\\W_EVT");
    assert_eq!(p.len(), 46);
    // payload_len 一致
    assert_eq!(
        u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize,
        46
    );
}

#[test]
fn test_host_render_hit_rect_layout() {
    use crate::protocol::HostRenderHitRect;
    let r = HostRenderHitRect {
        index: -1,
        x: 1,
        y: 2,
        w: 3,
        h: 4,
    };
    let b = r.to_bytes();
    assert_eq!(b.len(), 20);
    assert_eq!(i32::from_le_bytes(b[0..4].try_into().unwrap()), -1);
    assert_eq!(i32::from_le_bytes(b[16..20].try_into().unwrap()), 4);
}

#[test]
fn test_encode_hold_composition_layout() {
    let buf = encode_hold_composition(500, "，");
    // IpcHeader: 8 bytes (version u16 LE + cmd u16 LE + payload_len u32 LE)
    // payload: timeout_ms(4) + text_len(4) + "，"(3 UTF-8 bytes) = 11 bytes
    assert_eq!(buf.len(), 8 + 11);
    // cmd = 0x010A LE (at offset 2-4)
    assert_eq!(buf[2], 0x0A);
    assert_eq!(buf[3], 0x01);
    // payload_len = 11 LE (at offset 4-8)
    assert_eq!(u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]), 11);
    // timeout_ms = 500 LE (at offset 8-12)
    assert_eq!(u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]), 500);
    // text_len = 3 LE (at offset 12-16)
    assert_eq!(u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]), 3);
    // UTF-8 bytes of "，" = [0xEF, 0xBC, 0x8C] (at offset 16+)
    assert_eq!(&buf[16..], "，".as_bytes());
}

/// 智能符号 press2 的提交必须带 flags bit3，C++ 端据此改走「覆盖 held 符号」而非
/// 「并入前缀一起上屏」。漏置位的后果是 press2 打出「。.」而不是「.」。
#[test]
fn test_encode_commit_text_replacing_held_sets_flag_bit3() {
    let buf = encode_commit_text_replacing_held(".", true);
    let p = IpcHeader::SIZE;
    let flags = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
    assert_eq!(flags & 0x08, 0x08, "replacingHeld 位必须置位");
    assert_eq!(flags & 0x04, 0x04, "chineseMode 位应透传");
    assert_eq!(flags & 0x03, 0, "不得误置 modeChanged / hasNewComposition");
    // 命令码仍是 CMD_COMMIT_TEXT——复用同一响应类型，只靠 flags 区分语义
    assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), CMD_COMMIT_TEXT);
    assert_eq!(&buf[p + 12..], b".");
}

/// 反向守卫：普通提交路径绝不能带上 bit3，否则 hold 期间任何上屏都会吞掉待定符号。
#[test]
fn test_encode_commit_text_never_sets_replacing_held() {
    for (comp, mode_changed, cn, has_comp) in
        [(None, false, false, false), (Some("ni"), true, true, true)]
    {
        let buf = encode_commit_text("　", comp, mode_changed, cn, has_comp);
        let p = IpcHeader::SIZE;
        let flags = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
        assert_eq!(flags & 0x08, 0, "普通 CommitText 不得置 replacingHeld");
    }
}

#[test]
fn test_encode_commit_then_defer_layout() {
    let buf = encode_commit_then_defer(150, "可能", "y");
    let commit = "可能".as_bytes(); // 6 字节
    let defer = "y".as_bytes(); // 1 字节
    // header + timeout(4) + commit_len(4) + defer_len(4) + commit + defer
    assert_eq!(buf.len(), IpcHeader::SIZE + 12 + commit.len() + defer.len());
    // 命令码在 header 内
    let cmd = u16::from_le_bytes([buf[2], buf[3]]);
    assert_eq!(cmd, CMD_COMMIT_THEN_DEFER);
    // payload 起始处 timeout_ms
    let p = IpcHeader::SIZE;
    assert_eq!(
        u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]),
        150
    );
    assert_eq!(
        u32::from_le_bytes([buf[p + 4], buf[p + 5], buf[p + 6], buf[p + 7]]),
        commit.len() as u32
    );
    assert_eq!(
        u32::from_le_bytes([buf[p + 8], buf[p + 9], buf[p + 10], buf[p + 11]]),
        defer.len() as u32
    );
}

/// 占位档位按 kind 原样编成 1 字节；值与 C++ `PlaceholderForKind` 一一对应。
#[test]
fn composition_placeholder_value_is_the_kind_byte() {
    use crate::protocol::{
        COMPOSITION_PLACEHOLDER_BLANK, COMPOSITION_PLACEHOLDER_SPACE, COMPOSITION_PLACEHOLDER_ZWSP,
    };
    assert_eq!(COMPOSITION_PLACEHOLDER_SPACE, 0);
    assert_eq!(COMPOSITION_PLACEHOLDER_ZWSP, 1);
    assert_eq!(COMPOSITION_PLACEHOLDER_BLANK, 2);
    for k in [
        COMPOSITION_PLACEHOLDER_SPACE,
        COMPOSITION_PLACEHOLDER_ZWSP,
        COMPOSITION_PLACEHOLDER_BLANK,
    ] {
        assert_eq!(encode_composition_placeholder_value(k), vec![k]);
    }
}

/// 多行上屏方式：commit = 0、paste = 1，与 C++ 侧的判读一致。
#[test]
fn multiline_commit_value_is_one_byte() {
    assert_eq!(encode_multiline_commit_value(false), vec![0]);
    assert_eq!(encode_multiline_commit_value(true), vec![1]);
}
