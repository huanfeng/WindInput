use super::*;

fn now() -> Instant {
    Instant::now()
}

fn feed(buf: &mut DraftWindowBuf, text: &str, t: Instant) -> Vec<String> {
    buf.on_commit(text, t, DEFAULT_IDLE_TIMEOUT, 2, 5)
}

/// ★ 本模型的立身之本：**词组上屏不再中断造词**。
///
/// 「我」+「今天」+「去」——中间那个是选词组上屏的。旧模型在这里会把「我」结算掉、
/// 词组不参与、之后从零开始，于是攒不出任何跨词组的窗口。新模型必须切出「我今天」
/// 这种横跨单字与词组的组合，否则等于没解决主因。
#[test]
fn phrases_no_longer_break_the_stream() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "我", t);
    let w = feed(&mut b, "今天", t);
    assert!(
        w.contains(&"我今天".to_string()),
        "跨「单字→词组」的窗口必须切得出来，否则主因没解决：{w:?}"
    );
    let w = feed(&mut b, "去", t);
    assert!(w.contains(&"今天去".to_string()), "{w:?}");
    assert!(w.contains(&"我今天去".to_string()), "{w:?}");
}

/// 每次落屏只产出**以新字结尾**的窗口，长度落在 [min,max]。
#[test]
fn each_commit_yields_windows_ending_at_the_new_char() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    assert!(
        feed(&mut b, "一", t).is_empty(),
        "只有 1 个字，够不到 min=2"
    );
    assert_eq!(feed(&mut b, "二", t), vec!["一二"]);
    assert_eq!(feed(&mut b, "三", t), vec!["二三", "一二三"]);
    assert_eq!(feed(&mut b, "四", t), vec!["三四", "二三四", "一二三四"]);
    assert_eq!(
        feed(&mut b, "五", t),
        vec!["四五", "三四五", "二三四五", "一二三四五"]
    );
    // 第 6 个字：最长仍是 5，最早那个字已滑出
    assert_eq!(
        feed(&mut b, "六", t),
        vec!["五六", "四五六", "三四五六", "二三四五六"],
        "max=5 之外的窗口不该出现"
    );
}

/// 非汉字断流：标点两侧的字不能拼进同一个窗口。
#[test]
fn non_han_breaks_the_stream() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "上", t);
    feed(&mut b, "班", t);
    assert!(feed(&mut b, "。", t).is_empty(), "标点本身不产出窗口");
    let w = feed(&mut b, "今", t);
    assert!(w.is_empty(), "断流后重新起头，第一个字够不到 min");
    let w = feed(&mut b, "天", t);
    assert_eq!(w, vec!["今天"], "「班今天」这种跨句杂词不该出现：{w:?}");
}

/// 〇 不断流：「二〇二六」逐字上屏能拼出含 〇 的窗口，前面的字不被清掉。
#[test]
fn ling_does_not_break_the_stream() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "二", t);
    assert_eq!(feed(&mut b, "〇", t), vec!["二〇"]);
    assert_eq!(feed(&mut b, "二", t), vec!["〇二", "二〇二"]);
    assert!(feed(&mut b, "六", t).contains(&"二〇二六".to_string()));
}

/// idle 超时断流。
#[test]
fn idle_timeout_breaks_the_stream() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "上", t);
    let later = t + DEFAULT_IDLE_TIMEOUT + Duration::from_secs(1);
    let w = b.on_commit("班", later, DEFAULT_IDLE_TIMEOUT, 2, 5);
    assert!(w.is_empty(), "隔了很久再打，不该与上一段拼起来：{w:?}");
}

/// 回退按 **UTF-16 单元**，不是 char 数。
///
/// 扩展区汉字（`𠀀` U+20000）占 2 个单元。按 char 数回退会多删一个字，
/// 而码表里的生僻字大量落在扩展区——正是最需要造词的那批。
#[test]
fn rewind_counts_utf16_units_not_chars() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "工", t);
    feed(&mut b, "𠀀", t); // 2 个 UTF-16 单元
    assert_eq!(b.buffered(), "工𠀀");
    b.rewind(2); // 只该删掉那个扩展区字
    assert_eq!(b.buffered(), "工", "按 char 数回退会把「工」也删掉");
}

/// 回退量超过缓冲 ⇒ 清空。剩下的字与新文本之间的连续性已不可知。
#[test]
fn rewinding_past_the_buffer_clears_it() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "工", t);
    b.rewind(10);
    assert!(b.is_empty());
    let w = feed(&mut b, "人", t);
    assert!(w.is_empty(), "清空后是新段的第一个字，不该产出窗口：{w:?}");
}

/// 配置写反（min > max）表现为「窗口变少」，不是造词整个失灵。
#[test]
fn inverted_bounds_degrade_instead_of_disabling() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    b.on_commit("一", t, DEFAULT_IDLE_TIMEOUT, 4, 2);
    b.on_commit("二", t, DEFAULT_IDLE_TIMEOUT, 4, 2);
    b.on_commit("三", t, DEFAULT_IDLE_TIMEOUT, 4, 2);
    let w = b.on_commit("四", t, DEFAULT_IDLE_TIMEOUT, 4, 2);
    assert_eq!(
        w,
        vec!["一二三四"],
        "min>max 时该退化成只产出 min 长度：{w:?}"
    );
}

/// 0 = 用兜底值。
#[test]
fn zero_bounds_fall_back_to_defaults() {
    assert_eq!(
        normalize_bounds(0, 0),
        (DEFAULT_MIN_WINDOW, DEFAULT_MAX_WINDOW)
    );
}

/// 空文本不影响流（承自 `AutoPhraseBuf::empty_commit_is_noop`）。
///
/// 空的 `InsertText` 在生产里确实会出现（若干路径把「没有可上屏的文本」也走同一个
/// 返回变体）。它既不该产出窗口，也**不该被当成非汉字而断流**——那会让流被无端切碎。
#[test]
fn empty_commit_neither_yields_nor_breaks() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "你", t);
    assert!(feed(&mut b, "", t).is_empty(), "空文本不产出窗口");
    assert_eq!(b.buffered(), "你", "空文本不该影响流");
    assert_eq!(
        feed(&mut b, "好", t),
        vec!["你好"],
        "流没被切断，窗口照常切出"
    );
}

// ───────────── GH#181：窗口带上各字的上屏码 ─────────────

fn hinted(b: &mut DraftWindowBuf, text: &str, t: Instant) -> Vec<DraftWord> {
    b.on_commit_hinted(text, t, DEFAULT_IDLE_TIMEOUT, 2, 5)
}

/// 单字上屏前登记的码，随这个字进流，并出现在**所有**含它的窗口里。
#[test]
fn code_hint_rides_along_with_its_char() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    b.set_code_hint('好', "hc".into());
    hinted(&mut b, "好", t);
    b.set_code_hint('嘞', "lw".into());
    let w = hinted(&mut b, "嘞", t);
    assert_eq!(
        w,
        vec![DraftWord {
            text: "好嘞".into(),
            code_hints: vec![('好', "hc".into()), ('嘞', "lw".into())],
        }]
    );
    // 下一个字没登记码：窗口里只有前两个字带码。
    let w = hinted(&mut b, "啊", t);
    assert_eq!(
        w[1].code_hints,
        vec![('好', "hc".into()), ('嘞', "lw".into())],
        "{w:?}"
    );
    assert_eq!(w[0].text, "嘞啊");
    assert_eq!(w[0].code_hints, vec![('嘞', "lw".into())]);
}

/// 登记的码**只管下一次落屏**、且必须字对得上：词组上屏、换了个字上屏都不能张冠李戴。
#[test]
fn code_hint_applies_only_to_the_matching_single_char_commit() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    b.set_code_hint('今', "wyn".into());
    let w = hinted(&mut b, "今天", t);
    assert!(w[0].code_hints.is_empty(), "词组上屏不吃单字的码：{w:?}");
    b.set_code_hint('去', "fcu".into());
    let w = hinted(&mut b, "上", t);
    assert!(
        w.iter().all(|d| d.code_hints.is_empty()),
        "字对不上不能套用：{w:?}"
    );
    let w = hinted(&mut b, "去", t);
    assert!(
        w.iter().all(|d| d.code_hints.is_empty()),
        "登记只管下一次落屏，过期不再生效：{w:?}"
    );
}

/// 断流顺带作废未消费的登记。
#[test]
fn terminate_drops_pending_code_hint() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    hinted(&mut b, "你", t);
    b.set_code_hint('好', "vb".into());
    b.terminate();
    hinted(&mut b, "你", t);
    let w = hinted(&mut b, "好", t);
    assert!(w[0].code_hints.is_empty(), "{w:?}");
}

/// 断流不产出任何待结算的东西——与 `AutoPhraseBuf::terminate` 的语义分界。
#[test]
fn terminate_yields_nothing_because_windows_were_already_emitted() {
    let mut b = DraftWindowBuf::new();
    let t = now();
    feed(&mut b, "你", t);
    feed(&mut b, "好", t); // 「你好」这个窗口在这一刻就已经吐出去了
    b.terminate();
    assert!(b.is_empty());
}
