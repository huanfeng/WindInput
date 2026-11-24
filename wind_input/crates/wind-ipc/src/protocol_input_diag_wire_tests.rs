use super::*;

#[test]
fn key_payload_round_trips_seq_char() {
    let p = KeyPayload {
        key_code: 0x41,
        scan_code: 0,
        modifiers: 0,
        event_type: 0,
        toggles: 0,
        event_seq: 3,
        prev_char: 0x31,
        seq_char: 0x4F60,
    };
    let back = KeyPayload::from_bytes(&p.to_bytes()).expect("20 字节可解");
    assert_eq!({ back.prev_char }, 0x31);
    assert_eq!({ back.seq_char }, 0x4F60);
}

#[test]
fn key_payload_legacy_18_bytes_reads_seq_char_as_unknown() {
    // 旧 DLL / 旧 macOS 客户端的 18 字节包：照收，seq_char = 0（不可知）。
    let mut buf = [0u8; 18];
    buf[0..4].copy_from_slice(&0x41u32.to_le_bytes());
    buf[16..18].copy_from_slice(&0x35u16.to_le_bytes());
    let p = KeyPayload::from_bytes(&buf).expect("18 字节旧包仍可解");
    assert_eq!({ p.key_code }, 0x41);
    assert_eq!({ p.prev_char }, 0x35);
    assert_eq!({ p.seq_char }, 0);
    assert!(KeyPayload::from_bytes(&buf[..17]).is_none());
}

#[test]
fn focus_gained_backward_compat_36_bytes() {
    // 旧 36 字节载荷（无 disabled/reason）仍可解，新字段默认 0
    let mut buf = vec![0u8; 36];
    buf[20..28].copy_from_slice(&7u64.to_le_bytes()); // client_token
    buf[28..36].copy_from_slice(&(1u64 << 31).to_le_bytes()); // input_scope_mask
    let p = FocusGainedPayload::from_bytes(&buf).unwrap();
    assert_eq!(p.client_token, 7);
    assert_eq!(p.input_scope_mask, 1 << 31);
    assert_eq!(p.disabled, 0);
    assert_eq!(p.reason, 0);
}

#[test]
fn focus_gained_reads_new_fields_38_bytes() {
    let mut buf = vec![0u8; 38];
    buf[36] = 1; // disabled
    buf[37] = 2; // reason
    let p = FocusGainedPayload::from_bytes(&buf).unwrap();
    assert_eq!(p.disabled, 1);
    assert_eq!(p.reason, 2);
}

#[test]
fn key_observed_roundtrip() {
    let p = KeyObservedPayload {
        key_code: 0x41,
        modifiers: MOD_SHIFT,
    };
    let bytes = p.to_bytes();
    assert_eq!(bytes.len(), KeyObservedPayload::SIZE);
    assert_eq!(KeyObservedPayload::from_bytes(&bytes), Some(p));
    assert_eq!(KeyObservedPayload::from_bytes(&bytes[..7]), None);
}

#[test]
fn input_state_report_roundtrip() {
    let r = InputStateReportPayload {
        pid: 4242,
        disabled: 1,
        reason: 1,
        input_scope_mask: 1 << 31,
    };
    let bytes = r.to_bytes();
    assert_eq!(bytes.len(), InputStateReportPayload::SIZE);
    let d = InputStateReportPayload::from_bytes(&bytes).unwrap();
    assert_eq!(d.pid, 4242);
    assert_eq!(d.disabled, 1);
    assert_eq!(d.reason, 1);
    assert_eq!(d.input_scope_mask, 1 << 31);
}

fn sample_diag() -> DiagSnapshotPayload {
    DiagSnapshotPayload {
        pid: 4242,
        fg_pid: 777,
        focus_hwnd: 0x0000_0000_00A1_B2C3,
        root_hwnd: 0x0000_0001_1122_3344,
        fg_hwnd: 0x0000_0000_0055_6677,
        docmgr_id: 0x7FF6_1234_5678_9ABC,
        context_id: 0x7FF6_1234_5678_0000,
        focus_session_id: 19,
        root_band: 13,
        host_band: 6,
        focus_hwnd_source: window_source::TSF_VIEW,
        flags: DIAG_FLAG_DOCMGR_CHANGED,
        focus_class: "Edit".into(),
        root_class: "Shell_TrayWnd".into(),
        fg_class: "Windows.UI.Core.CoreWindow".into(),
    }
}

#[test]
fn diag_snapshot_roundtrip() {
    let p = sample_diag();
    let bytes = p.to_bytes();
    let d = DiagSnapshotPayload::from_bytes(&bytes).expect("应可解析");
    assert_eq!(d, p, "全字段往返必须一致");
    assert!(d.docmgr_changed());
    assert!(
        d.foreground_is_other_process(),
        "fg_pid≠pid 即前台属于别的进程——Win10 搜索框场景的关键信号"
    );
}

/// 头部偏移是两侧手写序列化的唯一契约（C++ 侧同样手写字节，没有编译期约束把它们绑住）。
/// 钉死总长与几个关键字段的落点，改布局时必须显式过这一关并同步 `BinaryProtocol.h`。
#[test]
fn diag_snapshot_head_layout_is_frozen() {
    let p = sample_diag();
    let b = p.to_bytes();
    assert_eq!(DiagSnapshotPayload::HEAD_SIZE, 64);
    assert_eq!(&b[0..4], &4242u32.to_le_bytes(), "pid @0");
    assert_eq!(&b[4..8], &777u32.to_le_bytes(), "fg_pid @4");
    assert_eq!(&b[48..52], &19u32.to_le_bytes(), "focus_session_id @48");
    assert_eq!(&b[52..56], &13u32.to_le_bytes(), "root_band @52");
    assert_eq!(b[60], window_source::TSF_VIEW, "focus_hwnd_source @60");
    assert_eq!(&b[62..64], &[0, 0], "reserved 必须留零");
    // 变长区紧跟头部：第一段是 focus_class。
    assert_eq!(&b[64..66], &4u16.to_le_bytes(), "focus_class 长度前缀");
    assert_eq!(&b[66..70], b"Edit");
}

/// 变长区被截断时只让对应类名退化为空串，头部字段与已完整的段必须照常可读。
/// 诊断数据缺一格也要能显示其余部分——整包否掉等于排查时两眼一抹黑。
#[test]
fn diag_snapshot_tolerates_truncated_tail() {
    let p = sample_diag();
    let full = p.to_bytes();
    // 砍到只剩头部 + 第一段类名
    let cut = 64 + 2 + 4;
    let d = DiagSnapshotPayload::from_bytes(&full[..cut]).expect("头部完整即应可解析");
    assert_eq!(d.root_band, 13, "头部字段不受尾部截断影响");
    assert_eq!(d.focus_class, "Edit", "完整的段照常可读");
    assert_eq!(d.root_class, "", "缺失的段退化为空串");
    assert_eq!(d.fg_class, "");
    // 头部本身不足则整包无效（无法信任任何字段）。
    assert!(DiagSnapshotPayload::from_bytes(&full[..63]).is_none());
}

#[test]
fn diag_snapshot_same_process_foreground_not_flagged() {
    let mut p = sample_diag();
    p.fg_pid = p.pid;
    assert!(!p.foreground_is_other_process());
    // pid 未知（0）时不得误报"跨进程"——那只是采集失败。
    p.fg_pid = 0;
    assert!(!p.foreground_is_other_process());
}
