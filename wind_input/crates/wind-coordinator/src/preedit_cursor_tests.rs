use super::*;

#[test]
fn map_pos_skips_inserted_separators() {
    // 引擎给的 display 在 buffer 基础上插入分词空格：ni|hao → "ni hao"
    assert_eq!(map_buf_pos_to_display_pos("nihao", "ni hao", 0), 0);
    assert_eq!(map_buf_pos_to_display_pos("nihao", "ni hao", 2), 2); // ni|hao → "ni| hao"
    assert_eq!(map_buf_pos_to_display_pos("nihao", "ni hao", 3), 4); // nih|ao → "ni h|ao"
    assert_eq!(map_buf_pos_to_display_pos("nihao", "ni hao", 5), 6); // 末尾
}

#[test]
fn map_pos_handles_apostrophe_display() {
    // 手动分隔符已在 buffer 里时，display 与 buffer 同形
    assert_eq!(map_buf_pos_to_display_pos("ni'hao", "ni'hao", 3), 3);
}

#[test]
fn map_pos_degenerate_display_falls_back() {
    // display 与 buffer 完全对不上（引擎回落/异常）：不 panic，退化为 display 长度内推进
    let got = map_buf_pos_to_display_pos("nihao", "", 3);
    assert_eq!(got, 0);
}

#[test]
fn caret_counts_prefix_in_utf16() {
    // 已转换前缀「你」= 1 个 UTF-16 单元；剩余 buffer "hao" 光标在 1 → display "h|ao"
    assert_eq!(caret_utf16("你", "hao", "hao", 1), 2);
    // 前缀为空
    assert_eq!(caret_utf16("", "nihao", "ni hao", 3), 4);
}

#[test]
fn caret_prefix_outside_bmp_counts_two_units() {
    // 扩展 B 区汉字（U+20000）占 2 个 UTF-16 单元——Go 按 rune 数会算成 1，这里必须是 2
    let ext = "\u{20000}";
    assert_eq!(ext.chars().count(), 1);
    assert_eq!(caret_utf16(ext, "hao", "hao", 0), 2);
}

#[test]
fn bufedit_insert_at_cursor() {
    let (mut b, mut c) = (String::from("niao"), 2usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    e.insert('h');
    assert_eq!((b.as_str(), c), ("nihao", 3));
}

#[test]
fn bufedit_insert_at_end_equals_push() {
    let (mut b, mut c) = (String::from("ni"), 2usize);
    BufEdit::new(&mut b, &mut c).insert('h');
    assert_eq!((b.as_str(), c), ("nih", 3));
}

#[test]
fn bufedit_backspace_and_delete() {
    let (mut b, mut c) = (String::from("nihao"), 3usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    assert!(e.backspace()); // 删光标前 'h'
    assert_eq!((b.as_str(), c), ("niao", 2));

    let mut e = BufEdit::new(&mut b, &mut c);
    assert!(e.delete()); // 删光标后 'a'，光标不动
    assert_eq!((b.as_str(), c), ("nio", 2));
}

#[test]
fn bufedit_boundaries_report_no_move() {
    let (mut b, mut c) = (String::from("ni"), 0usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    assert!(!e.move_left()); // 已在最左
    assert!(!e.backspace()); // 光标在 0，删不动
    assert!(e.move_right());
    assert!(e.move_right());
    assert!(!e.move_right()); // 已在最右
    assert!(!e.delete()); // 光标在末尾，前删无物
    assert_eq!(c, 2);
}

#[test]
fn bufedit_home_end() {
    let (mut b, mut c) = (String::from("nihao"), 2usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    assert!(e.home());
    assert_eq!(e.pos(), 0);
    assert!(!e.home()); // 已在最左
    assert!(e.end());
    assert_eq!(e.pos(), 5);
    assert!(!e.end());
}

#[test]
fn bufedit_clamps_stale_cursor() {
    // buffer 被别处整体替换、cursor 未同步（越界）：夹到边界而非 panic
    let (mut b, mut c) = (String::from("ni"), 99usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    assert_eq!(e.pos(), 2);
    assert!(e.backspace());
    assert_eq!(b.as_str(), "n");
}

/// 影子串按需起底：全小写输入不碰它（空 = 没有大写），出现第一个大写时才从当前缓冲成形。
#[test]
fn cased_shadow_materializes_only_on_first_uppercase() {
    let (mut b, mut c, mut cs) = (String::new(), 0usize, String::new());
    BufEdit::new_cased(&mut b, &mut c, &mut cs).insert_cased('a', 'a');
    assert_eq!((b.as_str(), cs.as_str()), ("a", ""), "全小写不该维护影子串");

    BufEdit::new_cased(&mut b, &mut c, &mut cs).insert_cased('b', 'B');
    assert_eq!((b.as_str(), cs.as_str()), ("ab", "aB"));
    BufEdit::new_cased(&mut b, &mut c, &mut cs).insert_cased('c', 'C');
    assert_eq!((b.as_str(), cs.as_str()), ("abc", "aBC"));
    assert_eq!(cased_or_buffer(&b, &cs), "aBC");
}

/// 退格/前删/光标中插：影子串与缓冲同步演进，形态不错位。
#[test]
fn cased_shadow_tracks_edits() {
    let (mut b, mut c, mut cs) = (String::from("abc"), 3usize, String::from("aBC"));
    assert!(BufEdit::new_cased(&mut b, &mut c, &mut cs).backspace());
    assert_eq!((b.as_str(), cs.as_str()), ("ab", "aB"));

    // 光标移到中间插入一个大写：两串同位置插入。
    BufEdit::new_cased(&mut b, &mut c, &mut cs).move_left();
    BufEdit::new_cased(&mut b, &mut c, &mut cs).insert_cased('x', 'X');
    assert_eq!((b.as_str(), cs.as_str()), ("axb", "aXB"));

    // Delete 删光标后一个（'b'/'B'），光标不动。
    assert!(BufEdit::new_cased(&mut b, &mut c, &mut cs).delete());
    assert_eq!((b.as_str(), cs.as_str()), ("ax", "aX"));
}

/// ★ 失配即退化：缓冲被别处清空/整体替换后，陈旧的影子串既不作数，也活不过下一次编辑。
/// 这是不去逐个接线二十余处缓冲写入点的**前提**——漏接的后果只能是丢大写，不能是串味。
#[test]
fn stale_cased_shadow_is_discarded_not_reused() {
    let (mut b, mut c, mut cs) = (String::from("abc"), 3usize, String::from("aBC"));
    // 别处清空了缓冲却没动影子串、也没动光标（模拟未接线的写入点）。
    b.clear();
    assert_eq!(cased_or_buffer(&b, &cs), "", "失配时读取必须退化到缓冲");
    assert!(!cased_is_valid(&b, &cs));

    // 重新打同样的三个字母（全小写）：构造时校准已把陈旧影子串清掉，不会读出 "aBC"。
    for ch in "abc".chars() {
        BufEdit::new_cased(&mut b, &mut c, &mut cs).insert_cased(ch, ch);
    }
    assert_eq!(cased_or_buffer(&b, &cs), "abc");
}

#[test]
fn project_case_onto_split_display() {
    // 拼音拆分串含引擎插入的分隔符：大写按序投影回去，分隔符原样保留。
    assert_eq!(project_case("nihao", "nIhao", "ni'hao"), "nI'hao");
    assert_eq!(project_case("abc", "aBC", "abc"), "aBC");
    // 影子串无效（空/失配）→ 原样返回，绝不臆造大写。
    assert_eq!(project_case("nihao", "", "ni'hao"), "ni'hao");
    assert_eq!(project_case("nihao", "aBC", "ni'hao"), "ni'hao");
}

#[test]
fn keep_cased_tail_follows_prefix_cut() {
    // 顶码留余码：缓冲被截成后缀，影子串同步掐头。
    let mut cs = String::from("aBCde");
    keep_cased_tail("de", &mut cs);
    assert_eq!(cs, "de");

    // 截出来的后缀对不上（缓冲被换成了别的内容）→ 清空退化，不留错位的大写。
    let mut cs = String::from("aBCde");
    keep_cased_tail("xy", &mut cs);
    assert!(cs.is_empty());
}

#[test]
fn bufedit_non_ascii_never_panics() {
    // 缓冲混入非 ASCII（当前不会发生，但保证不 panic 且按字符步进）
    let (mut b, mut c) = (String::from("中a"), 0usize);
    let mut e = BufEdit::new(&mut b, &mut c);
    assert!(e.move_right());
    assert_eq!(e.pos(), 3); // 「中」占 3 字节
    assert!(e.backspace());
    assert_eq!((b.as_str(), c), ("a", 0));
}

#[test]
fn syllable_spans_map_split_back_to_buffer() {
    assert_eq!(
        syllable_spans("fangxiang", "fang'xiang"),
        Some(vec![(0, 4), (4, 9)])
    );
    assert_eq!(
        syllable_spans("nihao", "ni'hao"),
        Some(vec![(0, 2), (2, 5)])
    );
    // 字母对不上（双拼展开成全拼）/ 没盖满缓冲 → None，调用方退回按字母移
    assert_eq!(syllable_spans("vs", "zhong"), None);
    assert_eq!(syllable_spans("fangxiangx", "fang'xiang"), None);
    assert_eq!(syllable_spans("", ""), None);
}

#[test]
fn stops_align_engine_split_and_typed_delimiter() {
    assert_eq!(syllable_stops("nihao", "ni'hao"), Some(vec![0, 2, 5]));
    assert_eq!(syllable_stops("ni'hao", "ni'hao"), Some(vec![0, 3, 6]));
    assert_eq!(syllable_stops("ni'hao", "nihao"), Some(vec![0, 3, 6]));
    assert_eq!(
        syllable_stops("woxihuan", "wo'xi'huan"),
        Some(vec![0, 2, 4, 8])
    );
    assert_eq!(syllable_stops("NiHao", "ni'hao"), Some(vec![0, 2, 5]));
    assert_eq!(syllable_stops("nihao", "ni'ha"), None, "没对满缓冲");
    assert_eq!(syllable_stops("nhk", "ni'hao"), None, "双拼：字母对不上");
}

#[test]
fn smart_jump_only_on_stops_else_char_step() {
    let s = [0, 3, 6]; // ni'hao
    assert_eq!(
        syllable_jump(&s, 0, true, true),
        Some(3),
        "头部边界 → 整音节"
    );
    assert_eq!(syllable_jump(&s, 3, true, true), Some(6));
    assert_eq!(syllable_jump(&s, 6, false, true), Some(3));
    assert_eq!(syllable_jump(&s, 3, false, true), Some(0));
    assert_eq!(syllable_jump(&s, 1, true, true), None, "音节中间 → 单字符");
    assert_eq!(
        syllable_jump(&s, 2, false, true),
        None,
        "分隔符之前不是停靠点"
    );
}

#[test]
fn jump_wraps_around_like_rime() {
    let s = [0, 3, 6];
    assert_eq!(
        syllable_jump(&s, 0, false, true),
        Some(3),
        "开头再左 → 最后音节起点"
    );
    assert_eq!(
        syllable_jump(&s, 6, true, true),
        Some(3),
        "末尾再右 → 第一个停靠点"
    );
}

#[test]
fn ctrl_jump_works_mid_syllable_and_needs_two_syllables() {
    let s = [0, 2, 5]; // ni hao
    assert_eq!(syllable_jump(&s, 1, false, false), Some(0));
    assert_eq!(syllable_jump(&s, 1, true, false), Some(2));
    assert_eq!(syllable_jump(&s, 3, false, false), Some(2));
    assert_eq!(syllable_jump(&s, 3, true, false), Some(5));
    assert_eq!(syllable_jump(&[0, 5], 2, true, false), None, "单音节不跳");
}
