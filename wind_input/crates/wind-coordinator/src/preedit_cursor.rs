//! 编码区（preedit）光标：缓冲区内的编辑位置，及「buffer 位置 → 显示串位置 → TSF caret」换算。
//!
//! 对齐 Go `internal/coordinator/pinyin_mode_shared.go` 与 `handle_candidates.go` 的光标模型：
//!
//! - 光标是 **buffer 内的字节偏移**，定义域 `[0, buf.len()]`。缓冲恒为 ASCII（拼音/五笔字母 +
//!   `'` 分隔符），故字节 == 字符 == UTF-16 单元；但本模块不假设这点，一律走 char boundary，
//!   避免将来缓冲混入非 ASCII 时 `String` 切片 panic（Go 切 `string` 越界只是运行时错，Rust
//!   切非边界直接 panic，这是移植中唯一必须收紧的地方）。
//! - 光标**不参与引擎查询**：`update_candidates` 恒以整串 buffer 查询，移动光标不重算候选。
//!   中间插入字符后按新的完整 buffer 全量重算，与在末尾输入无差别。
//! - 已转换前缀（`committed_text`）是只读前缀，光标进不去：Home 只到 buffer 开头。

/// 向下取到最近的字符边界（`i` 已在边界或越界时原样/截断返回）。
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 把 buffer 内的字节位置映射到 display 串内的字节位置。
///
/// 对齐 Go `mapBufferPosToDisplayPos`：逐字节贪心同步扫描——display 中与 buffer 对不上的字符
/// （引擎插入的音节分隔空格 / `'`）只推进 display 指针。不依赖音节边界表，故对「引擎怎么分词」
/// 无假设：只要 display 是 buffer 按序插入若干分隔符的产物，映射即成立。
pub(crate) fn map_buf_pos_to_display_pos(buffer: &str, display: &str, buf_pos: usize) -> usize {
    let (b, d) = (buffer.as_bytes(), display.as_bytes());
    let (mut bi, mut di) = (0usize, 0usize);
    while bi < buf_pos && di < d.len() {
        if bi < b.len() && d[di] == b[bi] {
            bi += 1;
        }
        di += 1;
    }
    di
}

/// 组合区光标的 TSF 单位（UTF-16 code unit）。
///
/// 组合区显示串 = `prefix`（已转换前缀/模式引导符，只读）+ `body`（buffer 的显示形态）。
/// `cursor` 是 `buffer` 内字节位，先映射到 body 内位置，再连同 prefix 一起折算为 UTF-16。
///
/// 单位取 UTF-16 而非字符数：C++ 侧 `TextService::_UpdateComposition` 用 `ITfRange::ShiftEnd`
/// 定位，其偏移单位是 UTF-16 code unit。Go 用 rune 数是因为汉字都在 BMP（见 Go
/// `displayCursorPos` 注释），扩展 B 区汉字（生僻字候选）会错位；Rust 直接按 UTF-16 算。
pub(crate) fn caret_utf16(prefix: &str, buffer: &str, body: &str, cursor: usize) -> u32 {
    let dpos = floor_boundary(body, map_buf_pos_to_display_pos(buffer, body, cursor));
    (prefix.encode_utf16().count() + body[..dpos].encode_utf16().count()) as u32
}

/// 组合区光标在**显示串内的字节偏移**（`prefix + body` 拼成的串），供自绘候选窗按字节切分
/// preedit 画插入符。与 [`caret_utf16`] 同源、只是单位不同——TSF 要 UTF-16 偏移，自绘窗要
/// UTF-8 字节以便 `&s[..n]` 切片（对齐 Go 的 `displayCursorPos` / `uiCursorPos` 之分）。
pub(crate) fn caret_display_bytes(prefix: &str, buffer: &str, body: &str, cursor: usize) -> usize {
    let dpos = floor_boundary(body, map_buf_pos_to_display_pos(buffer, body, cursor));
    prefix.len() + dpos
}

/// 「原始大小写影子串」是否作数：非空且与缓冲逐字符同形（仅 ASCII 字母大小写可能不同）。
///
/// 缓冲恒为全小写（引擎查询、顶码判定、词频记账都按它），用户按 Shift+字母打出的大写只存在
/// 影子串里。**校验而非信任**是这套结构的关键：缓冲有二十余处写入点（清空、整体替换、
/// 顶码截断…），逐个接线必然漏。失配即视为「没有大写」，于是漏接的后果是丢大写（显示退化成
/// 小写，功能照常），而不是把上一轮的大写错套到新缓冲上。
pub(crate) fn cased_is_valid(buffer: &str, cased: &str) -> bool {
    !cased.is_empty() && buffer.eq_ignore_ascii_case(cased)
}

/// 缓冲的「用户所打形态」：影子串有效则用它，否则用缓冲本身。供组合区显示与上屏原码。
pub(crate) fn cased_or_buffer<'a>(buffer: &'a str, cased: &'a str) -> &'a str {
    if cased_is_valid(buffer, cased) {
        cased
    } else {
        buffer
    }
}

/// 把影子串的大小写投影到显示串上（display = buffer 按序插入若干分隔符的产物，同
/// [`map_buf_pos_to_display_pos`] 的贪心同步扫描）。影子串无效时原样返回 display。
///
/// 大小写不改变字符数，故投影后 display 长度不变，caret 换算不受影响。
pub(crate) fn project_case(buffer: &str, cased: &str, display: &str) -> String {
    if !cased_is_valid(buffer, cased) {
        return display.to_string();
    }
    let (mut bc, mut cc) = (buffer.chars(), cased.chars());
    let mut pending = bc.next().zip(cc.next());
    let mut out = String::with_capacity(display.len());
    for dch in display.chars() {
        match pending {
            Some((b, c)) if dch == b => {
                out.push(c);
                pending = bc.next().zip(cc.next());
            }
            _ => out.push(dch),
        }
    }
    out
}

/// 缓冲被截成自身后缀之后（顶码留余码 / 分步确认消费前缀）同步影子串：掐掉同样长度的头部。
/// 对不上就清空——退化成全小写，好过让错位的大写留在缓冲上。
pub(crate) fn keep_cased_tail(buffer: &str, cased: &mut String) {
    if cased.is_empty() {
        return;
    }
    if cased.len() >= buffer.len() {
        let cut = cased.len() - buffer.len();
        if cased.is_char_boundary(cut) && buffer.eq_ignore_ascii_case(&cased[cut..]) {
            cased.replace_range(..cut, "");
            return;
        }
    }
    cased.clear();
}

/// 借用 buffer + cursor 的编辑视图，提供边界安全的编辑原语。
///
/// 所有 cursor 运算封死在此，调用方不做裸 `usize` 加减——这是 5 个 overlay 模式各写各的
/// `push`/`pop` 之外，唯一需要共享的东西。构造时把 cursor 夹到合法边界，故即便外部字段被
/// 别处直接改坏（如 buffer 整体替换后忘了重置 cursor），也不会 panic，只会退化到边界。
pub(crate) struct BufEdit<'a> {
    buf: &'a mut String,
    cursor: &'a mut usize,
    /// 与 buf 逐字符同形的「原始大小写」影子串（见 [`cased_is_valid`]）；`None` = 该缓冲
    /// 不支持大写（5 个 overlay 各自的缓冲），空串 = 支持但当前没有大写。
    cased: Option<&'a mut String>,
}

/// 把拼音拆分串（`fang'xiang`）对回缓冲（`fangxiang`），得到每个音节在**缓冲内**的字节区间。
///
/// 对不上（双拼的拆分串是展开后的全拼、字母与击键不同；缓冲里混了非字母）返回 `None`
/// ——调用方据此退回按字母移动，宁可不跳也不跳错。
pub(crate) fn syllable_spans(buffer: &str, split: &str) -> Option<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut at = 0usize;
    for syl in split.split('\'').filter(|s| !s.is_empty()) {
        let end = at + syl.len();
        if !buffer.is_char_boundary(end) || !buffer[at..end].eq_ignore_ascii_case(syl) {
            return None;
        }
        spans.push((at, end));
        at = end;
    }
    (at == buffer.len() && !spans.is_empty()).then_some(spans)
}

/// 左移到「上一个音节的起点」：光标在音节中间 → 该音节起点；恰在起点 → 前一个音节起点；
/// 已在最左 → 0。`fang'xiang` 光标在末尾，按一下落到 `xiang` 前，再按落到 `fang` 前。
pub(crate) fn syllable_left(spans: &[(usize, usize)], cursor: usize) -> usize {
    spans
        .iter()
        .map(|&(start, _)| start)
        .rfind(|&start| start < cursor)
        .unwrap_or(0)
}

impl<'a> BufEdit<'a> {
    pub(crate) fn new(buf: &'a mut String, cursor: &'a mut usize) -> Self {
        *cursor = floor_boundary(buf, *cursor);
        Self {
            buf,
            cursor,
            cased: None,
        }
    }

    /// 带影子串的编辑视图（普通输入的 `input_buffer` 专用）。**构造即校准**：影子串与缓冲
    /// 失配（别处清空或整体替换过缓冲）时就地清空，故陈旧的大写活不过下一次编辑。
    pub(crate) fn new_cased(
        buf: &'a mut String,
        cursor: &'a mut usize,
        cased: &'a mut String,
    ) -> Self {
        if !cased.is_empty() && !buf.eq_ignore_ascii_case(cased) {
            cased.clear();
        }
        *cursor = floor_boundary(buf, *cursor);
        Self {
            buf,
            cursor,
            cased: Some(cased),
        }
    }

    #[cfg(test)]
    pub(crate) fn pos(&self) -> usize {
        *self.cursor
    }

    /// 在光标处插入字符，光标随之后移（光标在末尾时等价于原来的 `push`）。
    pub(crate) fn insert(&mut self, ch: char) {
        self.insert_cased(ch, ch);
    }

    /// 插入字符：`ch` 进缓冲（引擎查询用，恒小写），`raw` 进影子串（用户所打的形态）。
    /// 两者相同且影子串还空着时**什么都不多做**——没有大写就不付维护成本，也就没有
    /// 「影子串悄悄跟缓冲分叉」的机会。
    ///
    /// `raw` 必须与 `ch` 等字节长（大小写变体），否则影子串与缓冲的字节位置会错开。
    pub(crate) fn insert_cased(&mut self, ch: char, raw: char) {
        debug_assert_eq!(raw.len_utf8(), ch.len_utf8());
        if let Some(cased) = self.cased.as_deref_mut()
            && (raw != ch || !cased.is_empty())
        {
            if cased.is_empty() {
                // 首个大写：影子串从当前缓冲（全小写）起底，此后与它同步演进。
                *cased = self.buf.clone();
            }
            cased.insert(*self.cursor, raw);
        }
        self.buf.insert(*self.cursor, ch);
        *self.cursor += ch.len_utf8();
    }

    /// 删除光标前一个字符（Backspace）。返回是否真的删了（光标在 0 → false）。
    pub(crate) fn backspace(&mut self) -> bool {
        if *self.cursor == 0 {
            return false;
        }
        let prev = floor_boundary(self.buf, *self.cursor - 1);
        self.cut_cased(prev..*self.cursor);
        self.buf.replace_range(prev..*self.cursor, "");
        *self.cursor = prev;
        true
    }

    /// 删除光标后一个字符（Delete），光标不动。返回是否真的删了（光标在末尾 → false）。
    pub(crate) fn delete(&mut self) -> bool {
        if *self.cursor >= self.buf.len() {
            return false;
        }
        let next = self.buf[*self.cursor..]
            .chars()
            .next()
            .map(|c| *self.cursor + c.len_utf8())
            .unwrap_or(self.buf.len());
        self.cut_cased(*self.cursor..next);
        self.buf.replace_range(*self.cursor..next, "");
        true
    }

    /// 影子串按与缓冲**相同的字节区间**同步删除（构造时已校准同形，故区间通用）。
    fn cut_cased(&mut self, range: std::ops::Range<usize>) {
        if let Some(cased) = self.cased.as_deref_mut()
            && !cased.is_empty()
        {
            cased.replace_range(range, "");
        }
    }

    /// 左移一个字符。返回是否移动了（已在最左 → false，调用方据此决定吃键还是透传）。
    pub(crate) fn move_left(&mut self) -> bool {
        if *self.cursor == 0 {
            return false;
        }
        *self.cursor = floor_boundary(self.buf, *self.cursor - 1);
        true
    }

    /// 右移一个字符。返回是否移动了（已在最右 → false）。
    pub(crate) fn move_right(&mut self) -> bool {
        if *self.cursor >= self.buf.len() {
            return false;
        }
        *self.cursor = self.buf[*self.cursor..]
            .chars()
            .next()
            .map(|c| *self.cursor + c.len_utf8())
            .unwrap_or(self.buf.len());
        true
    }

    /// 跳到最左。返回是否移动了。
    pub(crate) fn home(&mut self) -> bool {
        let moved = *self.cursor != 0;
        *self.cursor = 0;
        moved
    }

    /// 跳到最右。返回是否移动了。
    pub(crate) fn end(&mut self) -> bool {
        let moved = *self.cursor != self.buf.len();
        *self.cursor = self.buf.len();
        moved
    }
}

#[cfg(test)]
mod tests {
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
    fn syllable_left_jumps_to_previous_syllable_start() {
        let spans = [(0, 4), (4, 9), (9, 12)]; // fang xiang yao
        assert_eq!(syllable_left(&spans, 12), 9);
        assert_eq!(syllable_left(&spans, 9), 4, "恰在起点 → 前一个音节");
        assert_eq!(syllable_left(&spans, 7), 4, "在音节中间 → 该音节起点");
        assert_eq!(syllable_left(&spans, 4), 0);
        assert_eq!(syllable_left(&spans, 0), 0);
    }
}
