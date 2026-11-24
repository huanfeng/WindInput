//! 编码区（preedit）光标：缓冲区内的编辑位置，及「buffer 位置 → 显示串位置 → TSF caret」换算。
//!
//! - 光标是 buffer 内的字节偏移，定义域 `[0, buf.len()]`。缓冲目前恒为 ASCII，但本模块不假设
//!   这点，一律走 char boundary，免得将来混入非 ASCII 时切片 panic。
//! - 光标不参与引擎查询：`update_candidates` 恒以整串 buffer 查询，移动光标不重算候选。
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
/// 单位取 UTF-16：C++ 侧 `_UpdateComposition` 用 `ITfRange::ShiftEnd` 定位，按字符数算扩展区
/// 汉字会错位。
pub(crate) fn caret_utf16(prefix: &str, buffer: &str, body: &str, cursor: usize) -> u32 {
    let dpos = floor_boundary(body, map_buf_pos_to_display_pos(buffer, body, cursor));
    (prefix.encode_utf16().count() + body[..dpos].encode_utf16().count()) as u32
}

/// 组合区光标在显示串内的字节偏移（`prefix + body` 拼成的串），供自绘候选窗按字节切分
/// preedit 画插入符。与 [`caret_utf16`] 同源、只是单位不同——TSF 要 UTF-16 偏移，自绘窗要
/// UTF-8 字节以便 `&s[..n]` 切片（对齐 Go 的 `displayCursorPos` / `uiCursorPos` 之分）。
pub(crate) fn caret_display_bytes(prefix: &str, buffer: &str, body: &str, cursor: usize) -> usize {
    let dpos = floor_boundary(body, map_buf_pos_to_display_pos(buffer, body, cursor));
    prefix.len() + dpos
}

/// 「原始大小写影子串」是否作数：非空且与缓冲逐字符同形（仅 ASCII 字母大小写可能不同）。
///
/// 缓冲恒为全小写，用户打出的大写只存在影子串里。校验而非信任：缓冲有二十余处写入点，逐个
/// 接线必然漏；失配即视为「没有大写」，漏接的后果只是显示退化成小写，不会错套上一轮的大写。
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

/// 把拼音拆分串（`fang'xiang`）对回缓冲（`fangxiang`），得到每个音节在缓冲内的字节区间。
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

/// 把拆分串（`ni'hao`）对回缓冲，得到音节起点在缓冲内的字节位置（升序、含 0 与缓冲末尾）。
///
/// 与 [`syllable_spans`] 的区别：缓冲里手敲的分隔符 `'` 也算数——拆分串里有没有这个 `'` 都行，
/// 起点取分隔符之后（rime 默认 `after_delimiter`），故 `ni'hao` 的停靠点是 `{0, 3, 6}`，
/// 光标在 `ni|'hao`（分隔符之前）不是停靠点。对不上（双拼、缓冲里混了拆分串没有的字符）返回
/// `None`，调用方退回按字符移动。
pub(crate) fn syllable_stops(buffer: &str, split: &str) -> Option<Vec<usize>> {
    let (b, mut at) = (buffer.as_bytes(), 0usize);
    let mut stops = vec![0usize];
    for ch in split.chars() {
        if ch == '\'' {
            if b.get(at) == Some(&b'\'') {
                at += 1;
            }
            stops.push(at);
        } else {
            if b.get(at) == Some(&b'\'') {
                at += 1;
                stops.push(at);
            }
            let len = ch.len_utf8();
            let piece = buffer.get(at..at + len)?;
            if !piece.eq_ignore_ascii_case(ch.encode_utf8(&mut [0; 4])) {
                return None;
            }
            at += len;
        }
    }
    if at != buffer.len() {
        return None;
    }
    stops.push(at);
    stops.dedup();
    Some(stops)
}

/// rime 的音节跳转（`Navigator::JumpLeft` / `JumpRight`，`syllable_jump_position: after_delimiter`）。
///
/// - 向左：落到前一个停靠点；已在开头则回绕到最后一个音节的起点；
/// - 向右：落到后一个停靠点；已在末尾则回绕到第一个音节之后的停靠点；
/// - 不足两个音节（`stops` 少于 3 个）没有「跳」可言，返回 `None`。
///
/// `smart` = 真：rime 默认的 ←/→（`Rewind` / `Forward`）——光标不在停靠点上（音节中间）时返回
/// `None`，由调用方按单字符移动；假：Ctrl+←/→（`left_by_syllable` / `right_by_syllable`），
/// 音节中间也直接跳到最近的停靠点。
pub(crate) fn syllable_jump(
    stops: &[usize],
    cursor: usize,
    right: bool,
    smart: bool,
) -> Option<usize> {
    if stops.len() < 3 {
        return None;
    }
    let (first, last) = (stops[0], stops[stops.len() - 1]);
    if smart && !stops.contains(&cursor) {
        return None;
    }
    let target = if right {
        if cursor >= last {
            stops[1]
        } else {
            *stops.iter().find(|&&p| p > cursor)?
        }
    } else if cursor <= first {
        stops[stops.len() - 2]
    } else {
        *stops.iter().rfind(|&&p| p < cursor)?
    };
    (target != cursor).then_some(target)
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

    /// 带影子串的编辑视图（普通输入的 `input_buffer` 专用）。构造即校准：影子串与缓冲
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

    pub(crate) fn pos(&self) -> usize {
        *self.cursor
    }

    /// 直接落到 `pos`（调用方保证是字符边界，如 [`syllable_jump`] 的停靠点）。
    pub(crate) fn set_pos(&mut self, pos: usize) {
        *self.cursor = floor_boundary(self.buf, pos);
    }

    /// 在光标处插入字符，光标随之后移（光标在末尾时等价于原来的 `push`）。
    pub(crate) fn insert(&mut self, ch: char) {
        self.insert_cased(ch, ch);
    }

    /// 插入字符：`ch` 进缓冲（引擎查询用，恒小写），`raw` 进影子串（用户所打的形态）。
    /// 两者相同且影子串还空着时什么都不多做——没有大写就不付维护成本，也就没有
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

    /// 影子串按与缓冲相同的字节区间同步删除（构造时已校准同形，故区间通用）。
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
#[path = "preedit_cursor_tests.rs"]
mod tests;
