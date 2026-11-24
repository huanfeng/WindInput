use super::*;

/// 编码是最容易写错的一环（位运算 + 单向转义），用已知取值锁死。
/// 期望值来自对 `gram_encoding.cc` 的逐行复算，并已在真实 `.gram` 上
/// 用「查得到高频搭配」交叉验证过（设计文档 §2.2.3 记了 8/10 命中）。
#[test]
fn encode_cjk_main_block_to_two_bytes() {
    let mut buf = EncodeBuf::default();

    // 的 U+7684 落在 [0x4000,0xA000)：(0x76 + 0x40, 0x84)
    assert_eq!(encode_chars("的".chars(), 8, &mut buf), 1);
    assert_eq!(buf.as_slice(), &[0xB6, 0x84]);

    // 时 U+65F6 → (0x65 + 0x40, 0xF6)
    assert_eq!(encode_chars("时".chars(), 8, &mut buf), 1);
    assert_eq!(buf.as_slice(), &[0xA5, 0xF6]);

    // 多字符拼接：候 U+5019 → (0x90, 0x19)
    assert_eq!(encode_chars("时候".chars(), 8, &mut buf), 2);
    assert_eq!(buf.as_slice(), &[0xA5, 0xF6, 0x90, 0x19]);
}

/// ASCII 原样单字节——octagram 用 `$` 作句末标记，靠的就是这条。
#[test]
fn encode_ascii_stays_one_byte() {
    let mut buf = EncodeBuf::default();
    assert_eq!(encode_chars("$".chars(), 8, &mut buf), 1);
    assert_eq!(buf.as_slice(), b"$");
}

/// `max_chars` 与 `MAX_ENCODED_UNICODE` 双重截断，缓冲区不会溢出。
#[test]
fn encode_respects_char_limit() {
    let mut buf = EncodeBuf::default();
    assert_eq!(encode_chars("时候时候".chars(), 2, &mut buf), 2);
    assert_eq!(buf.as_slice(), &[0xA5, 0xF6, 0x90, 0x19]);

    let long: String = "字".repeat(50);
    let n = encode_chars(long.chars(), 999, &mut buf);
    assert_eq!(n, MAX_ENCODED_UNICODE, "不得超过 kMaxEncodedUnicode");
    assert!(buf.as_slice().len() <= ENCODE_BUF_LEN);
}

/// 编码串的「按字符前进」要与编码规则互逆（CJK 2 字节 / ASCII 1 字节）。
#[test]
fn encoded_unicode_len_counts_chars_not_bytes() {
    let mut buf = EncodeBuf::default();
    encode_chars("时候".chars(), 8, &mut buf);
    let s = buf.as_slice();
    assert_eq!(s.len(), 4);
    assert_eq!(GramDb::encoded_unicode_len(s, 4), 2);
    assert_eq!(GramDb::encoded_unicode_len(s, 2), 1);
}
