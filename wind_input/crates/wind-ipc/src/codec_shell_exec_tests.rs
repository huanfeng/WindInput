use super::*;

/// 按「长度前缀 + 内容」逐段解码，最多取 `max_segs` 段。
/// `max_segs=2` 即模拟只认 target/params 的**旧版 DLL**。
fn decode_segs(buf: &[u8], max_segs: usize) -> Vec<String> {
    let mut p = &buf[IpcHeader::SIZE..];
    let mut out = Vec::new();
    while out.len() < max_segs && p.len() >= 4 {
        let n = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
        if p.len() < 4 + n {
            break;
        }
        out.push(String::from_utf8(p[4..4 + n].to_vec()).unwrap());
        p = &p[4 + n..];
    }
    out
}

#[test]
fn encodes_five_segments_in_order() {
    let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
    assert_eq!(
        decode_segs(&buf, 5),
        vec!["d.exe", "a b", "D:/W", "runas", "min"]
    );
}

/// 未指定的选项走空串段而非省略段——段数恒定，解码方不必猜"少的是哪一个"。
#[test]
fn omitted_options_are_empty_segments_not_missing_ones() {
    let buf = encode_shell_exec("d.exe", "", "", "", "");
    assert_eq!(decode_segs(&buf, 5), vec!["d.exe", "", "", "", ""]);
}

/// 向后兼容：只认前两段的旧 DLL 读新报文，仍拿到正确的 target/params。
#[test]
fn old_two_segment_reader_still_gets_target_and_params() {
    let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
    assert_eq!(decode_segs(&buf, 2), vec!["d.exe", "a b"]);
}

/// 载荷长度必须与声明一致，否则 C++ 侧以 header.length 为边界会截断尾部段。
#[test]
fn payload_len_in_header_covers_all_segments() {
    let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
    let declared = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    assert_eq!(declared, buf.len() - IpcHeader::SIZE);
}

/// 非 ASCII 段按 UTF-8 字节计长（中文路径必经之路）。
#[test]
fn non_ascii_segments_use_byte_length() {
    let buf = encode_shell_exec("d.exe", "", "D:/我的 词库", "", "");
    assert_eq!(decode_segs(&buf, 3)[2], "D:/我的 词库");
}
