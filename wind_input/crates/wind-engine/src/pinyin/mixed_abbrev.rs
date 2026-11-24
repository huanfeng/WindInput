//! 混合简拼：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
//!
//! 设计文档：`docs/design/pinyin-mixed-abbrev.md`。
//!
//! 只产「模式」不产「切分」（文档 §4.5 方案 D）：
//!
//! ```text
//!   nhao ──切分──> [Initial('n'), Syllable("hao")]   ← 模式（本模块）
//!        ──投影──> "nh"                              ← 声母串，正是 AbbrevSection 现有的键
//!        ──点查──> "nihao"（真值全拼码，非推断）
//!        ──校验──> ni|hao 逐段比对：n? ✓ / ==hao ✓   ← 模式在这里第二次发挥作用
//! ```
//!
//! 投影键退化成纯简拼，索引不用改；混合信息留在模式里做后置校验。
//!
//! ## 与纯简拼的分工
//!
//! 本模块认**既有声母段又有音节段**的解释，外加**含双字母声母**的那些：
//! - 全是单字母声母段（`nh`）→ 纯简拼，由 `AbbrevMatcher` + step5 处理，这里返回空；
//! - 全是音节段（`nihao`）→ 全拼，走主路径；
//! - 含 `zh`/`ch`/`sh` 段（`zhy` = zh|y）→ 归本模块，即使全是声母段：纯简拼逐字母切，
//!   表达不了「zh 是一个声母」（「这样」挂在键 `zy` 下）。
//!
//! 调用方还应先确认整串**不能**被完整切成音节序列，否则常见全拼输入会白跑一趟（见
//! `PinyinEngine::convert` step 5b 的短路）。

use super::syllable::SyllableTrie;

/// 模式最大段数。与 `AbbrevMatcher::find_candidates` 的简拼上限（6）一致——
/// 再长的词打混合简拼已无收益，而段数直接决定 DFS 深度。
const MAX_SEGMENTS: usize = 6;

/// 单串最多保留的模式数。达到上限即停止枚举（DFS 顺序固定，截断确定：长音节 > 短音节 >
/// 双字母声母 > 单字母声母，更具体的解释先保留）。
///
/// 取 24 给双字母声母那一档留出额度（`chengshizhong` 这类串解释最多约 16 条），仍远低于
/// 成本拐点（`tests/pinyin_abbrev_recall_latency.rs`）。
const MAX_PATTERNS: usize = 24;

/// 超过此长度不做混合解释。长串的合法解释本就少，而枚举成本随长度增长。
const MAX_INPUT_LEN: usize = 16;

/// 混合简拼的一段：一个声母（单字母或 `zh`/`ch`/`sh`），或一个完整音节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbbrevSeg {
    /// 声母段：只约束对应音节的**首字母**。
    Initial(char),
    /// 双字母声母段（`zh`/`ch`/`sh`）：约束对应音节以这**两个**字母开头。
    /// 载荷是 z/c/s 那一位 —— 投影键取的正是它，故索引不受影响（`zh|ge` → 键 `zg`）。
    ///
    /// 独立成段而非 [`Initial`](Self::Initial) 的特例：二者消耗的击键数不同（2 vs 1）。
    Retroflex(char),
    /// 音节段：约束对应音节**全等**。
    Syllable(String),
}

impl AbbrevSeg {
    /// 本段与词典音节的**精确**匹配判定 —— 段语义的唯一真相源。
    ///
    /// 模糊放宽由调用方在此基础上叠加（见 `PinyinEngine::seg_matches_fuzzy`）：先问这里，
    /// 不中再试变体。
    pub fn matches_exact(&self, syl: &str) -> bool {
        match self {
            AbbrevSeg::Initial(c) => syl.starts_with(*c),
            AbbrevSeg::Retroflex(c) => {
                let mut it = syl.chars();
                it.next() == Some(*c) && it.next() == Some('h')
            }
            AbbrevSeg::Syllable(s) => syl == s,
        }
    }
}

/// 一串输入的一种混合解释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixedPattern {
    segs: Vec<AbbrevSeg>,
    /// 各段首字母拼接 —— 即 `AbbrevSection` 的索引键（见模块文档）。
    key: String,
}

impl MixedPattern {
    fn new(segs: Vec<AbbrevSeg>) -> Self {
        let key = segs
            .iter()
            .map(|s| match s {
                AbbrevSeg::Initial(c) | AbbrevSeg::Retroflex(c) => *c,
                // 音节段非空（来自 trie 匹配），first() 必有值
                AbbrevSeg::Syllable(s) => s.chars().next().unwrap_or('?'),
            })
            .collect();
        Self { segs, key }
    }

    /// 声母投影键：拿它查 `AbbrevSection`（键是完整简拼串，本模式退化后正好对上）。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 段数 —— 也就是这条模式要求候选词有几个音节。
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// 候选词的音节序列是否符合本模式。
    ///
    /// 音节数（段数）相等是硬条件（文档 §5 约束 3）：扁平码有损，不卡住会捞出高权重单字。
    ///
    /// 生产代码已无调用点（都走 [`Self::matches_with`] 拿模糊处数），只作单测入口；单测不用了
    /// 就删，别接回生产路径。
    pub fn matches<S: AsRef<str>>(&self, syllables: &[S]) -> bool {
        self.matches_with(syllables, |seg, syl| seg.matches_exact(syl).then_some(0))
            .is_some()
    }

    /// 同 [`Self::matches`]，但音节段的比较交给调用方 —— 供模糊音放宽。
    ///
    /// 闭包对每一段返回 `Some(edits)`（模糊改动处数，`0` = 精确）或 `None`（不匹配），整条
    /// 模式返回各段之和。调用方必须据此施加 [`fuzzy_penalized`](crate::pinyin) 折扣并标
    /// `is_fuzzy`（模糊命中恒低精确一档是全仓不变量）；返回处数而非 `bool` 就是为了不让它被忽略。
    ///
    /// 三种段都要过钩子：音节段（`senrikl` 的 `sen` 要配 `sheng`）、改首字母的声母组
    /// （n↔l 等）、双字母声母段的卷舌放宽。方向同 `PinyinEngine::fuzzy_codes`：扩展用户输入段
    /// 去匹配词典音节。
    pub fn matches_with<S: AsRef<str>>(
        &self,
        syllables: &[S],
        seg_match: impl Fn(&AbbrevSeg, &str) -> Option<usize>,
    ) -> Option<usize> {
        if syllables.len() != self.segs.len() {
            return None;
        }
        let mut edits = 0usize;
        for (seg, syl) in self.segs.iter().zip(syllables) {
            edits += seg_match(seg, syl.as_ref())?;
        }
        Some(edits)
    }
}

/// 枚举 `input` 的全部混合解释（既含声母段又含音节段的那些）。
///
/// 判据侧的注意事项（文档 §5 约束 4）：调用方须传**原始击键**，不是双拼转换后的全拼——
/// 混合简拼和纯简拼一样，讲的是用户敲下的字母，与编码方案无关。
pub fn mixed_patterns(input: &str, trie: &SyllableTrie) -> Vec<MixedPattern> {
    if input.len() < 2
        || input.len() > MAX_INPUT_LEN
        || !input.bytes().all(|b| b.is_ascii_lowercase())
    {
        return Vec::new();
    }

    // 可达性预筛：`reach[i]` = 从位置 i 出发能否恰好走到串尾。
    // 没有它，DFS 会把大量走不到头的死胡同也遍历一遍（`zhongguoren` 这类长串尤其明显）；
    // 有了它，DFS 只走真实存在的完整路径，遍历量与产出的模式数同阶。
    // 注意本表**不含段数上限**，故 DFS 仍需自行按 MAX_SEGMENTS 剪枝。
    let n = input.len();
    let mut reach = vec![false; n + 1];
    reach[n] = true;
    for pos in (0..n).rev() {
        if edges(input, pos, trie).any(|e| reach[pos + e.len()]) {
            reach[pos] = true;
        }
    }
    if !reach[0] {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut cur = Vec::new();
    walk(input, 0, trie, &reach, &mut cur, &mut out);
    out
}

/// 一条出边：吃掉几个字节，以及吃成哪种段。
///
/// 必须带类型而不能只给长度：`zh` 与 `ba` 都是 2 字节，一个是声母段、一个是音节段。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Initial,
    Retroflex,
    Syllable(usize),
}

impl Edge {
    fn len(self) -> usize {
        match self {
            Edge::Initial => 1,
            Edge::Retroflex => 2,
            Edge::Syllable(n) => n,
        }
    }
}

/// 位置 `pos` 上的所有出边：完整音节（长→短）在前，然后双字母声母，最后单字母声母。
///
/// 顺序即 DFS 的保留优先级（见 `MAX_PATTERNS`）：更具体的解释先被保留。双字母声母排在
/// 单字母之前，因为 `zh` 比 `z` 约束更强。
///
/// **长度为 1 的音节不作为音节边**（`a`/`e`/`o`）：它与同位置的声母边消耗一样多的字节，
/// 而声母边的约束更松（`a?` ⊇ `==a`），保留两条只是把同一批词查两遍。
fn edges<'a>(
    input: &'a str,
    pos: usize,
    trie: &'a SyllableTrie,
) -> impl Iterator<Item = Edge> + 'a {
    // 换位纠错拼写不作音节段：`zhon` 成了整串音节段，整串混合简拼便算「命中」，
    // 关纠错时才有的前缀简拼降级（「之后」「中欧」）整批不再触发。纠错只能加候选。
    let syls = trie.match_at(input, pos).into_iter().filter_map(|s| {
        let n = s.len();
        (n > 1 && !trie.is_correction_spelling(&s)).then_some(Edge::Syllable(n))
    });
    let retroflex = is_retroflex_at(input, pos, trie).then_some(Edge::Retroflex);
    let initial = is_initial(input.as_bytes()[pos], trie).then_some(Edge::Initial);
    syls.chain(retroflex).chain(initial)
}

/// 该字母是否可作声母段 —— 判据与 `AbbrevMatcher::is_abbreviation` 同款：存在以它开头的音节。
fn is_initial(byte: u8, trie: &SyllableTrie) -> bool {
    trie.is_prefix(std::str::from_utf8(&[byte]).unwrap_or(""))
}

/// `pos` 处是不是 `zh`/`ch`/`sh`。
///
/// 向 trie 求证而非硬编码三个字面量：音节表是真相源。
fn is_retroflex_at(input: &str, pos: usize, trie: &SyllableTrie) -> bool {
    let b = input.as_bytes();
    if pos + 1 >= b.len() || b[pos + 1] != b'h' {
        return false;
    }
    trie.is_prefix(&input[pos..pos + 2])
}

fn walk(
    input: &str,
    pos: usize,
    trie: &SyllableTrie,
    reach: &[bool],
    cur: &mut Vec<AbbrevSeg>,
    out: &mut Vec<MixedPattern>,
) {
    if out.len() >= MAX_PATTERNS {
        return;
    }
    if pos == input.len() {
        // 两种退化形态不归本模块：全**单字母**声母 = 纯简拼（step5），全音节 = 全拼（主路径）。
        //
        // 含 `Retroflex` 段的模式即使全是声母段也要收（`zhy`/`zhsh`），见模块文档。
        let has_retroflex = cur.iter().any(|s| matches!(s, AbbrevSeg::Retroflex(_)));
        let has_initial = cur
            .iter()
            .any(|s| matches!(s, AbbrevSeg::Initial(_) | AbbrevSeg::Retroflex(_)));
        let has_syllable = cur.iter().any(|s| matches!(s, AbbrevSeg::Syllable(_)));
        // 投影键至少 2 位（段数 ≥ 2）：单段 `zh` 只投影键 `z`，会绕过 `MIN_ABBREV_STROKE`、
        // 把 zh 开头的高频单字灌进候选（`zhq` 在 step 6.2 的 `zh` 切点上）。
        if cur.len() >= 2 && (has_retroflex || (has_initial && has_syllable)) {
            out.push(MixedPattern::new(cur.clone()));
        }
        return;
    }
    if cur.len() >= MAX_SEGMENTS {
        return;
    }
    for edge in edges(input, pos, trie).collect::<Vec<_>>() {
        let len = edge.len();
        if !reach[pos + len] {
            continue;
        }
        cur.push(match edge {
            Edge::Initial => AbbrevSeg::Initial(input.as_bytes()[pos] as char),
            Edge::Retroflex => AbbrevSeg::Retroflex(input.as_bytes()[pos] as char),
            Edge::Syllable(_) => AbbrevSeg::Syllable(input[pos..pos + len].to_string()),
        });
        walk(input, pos + len, trie, reach, cur, out);
        cur.pop();
        if out.len() >= MAX_PATTERNS {
            return;
        }
    }
}

/// 按 `boundary`（音节起始**字节**位 bitmask）把全拼码切回音节序列。
///
/// 混合校验唯一的音节来源，故 `boundary == 0` 一律返回 `None`、不参与混合简拼。与全仓
/// 「任一侧为 0 即降级放行」相反：这里 boundary 缺失等于判据不存在，放行就成了不校验。
///
/// ⚠️ 与 [`super::PinyinEngine::abbrev_of_code`] **必须对同一个 boundary 给出一致的解释**
/// （那边取的正是这里每段的首字母）。改动其一时同步核对另一处。
pub fn syllables_from_boundary(code: &str, boundary: u64) -> Option<Vec<&str>> {
    // bit0 未置位 = 第一个音节不从 0 开始 —— 坏数据，不猜。
    //
    // `!code.is_ascii()`：下面按字节切片，非 ASCII 会 panic；调用面含每条拼音候选
    // （`shuangpin_code_of`，按键线程持锁），panic 即输入法崩溃。
    if boundary & 1 == 0 || code.is_empty() || !code.is_ascii() {
        return None;
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 1..code.len().min(64) {
        if (boundary >> i) & 1 == 1 {
            out.push(&code[start..i]);
            start = i;
        }
    }
    out.push(&code[start..]);
    Some(out)
}

/// 按候选的真值音节序列，把**击键串**切成对应的段并以 `'` 连接
/// （`nhao` + `ni|hao` → `n'hao`；`nh` + `ni|hao` → `n'h`）。
///
/// 不能直接渲染候选的 code：preedit 必须与击键串同域，`nh` 显示成 `ni'hao` 会让退格与光标错位
/// （文档 §4.6）。
///
/// 逐音节贪心：能整段对上该音节就是音节段，否则是声母段（1 字节，或 `zh`/`ch`/`sh` 2 字节）。
/// 双字母声母要试两遍：`zhge`+zhe|ge 应切 `zh'ge`，`zh`+zhong|hua 应切 `z'h`，贪心无回溯，
/// 故先双字母优先、不成再单字母。能整串对齐的切法唯一，两遍不会给出不同结果。
///
/// 返回 `(渲染串, 已消费的 raw 字节数)`；部分匹配（step 6.2 前缀回退）时余下字母由调用方
/// 自己再切（文档 §4.7.2）。对不上一律返回 `None`。不变量：去掉 `'` 必须恰好还原击键串。
pub fn render_keystroke_preedit(raw: &str, syllables: &[&str]) -> Option<(String, usize)> {
    if raw.is_empty() || syllables.is_empty() || !raw.is_ascii() {
        return None;
    }
    render_pass(raw, syllables, true).or_else(|| render_pass(raw, syllables, false))
}

/// [`render_keystroke_preedit`] 的一遍扫描。`allow_two` 决定这一遍认不认双字母声母。
fn render_pass(raw: &str, syllables: &[&str], allow_two: bool) -> Option<(String, usize)> {
    let mut out = String::with_capacity(raw.len() + syllables.len());
    let mut pos = 0usize;
    for (i, syl) in syllables.iter().enumerate() {
        if pos >= raw.len() {
            return None; // 音节比击键还多
        }
        if i > 0 {
            out.push('\'');
        }
        // 贪心：整段音节 > 双字母声母 > 单字母声母。顺序与 `edges` 一致（更具体的先试）。
        //
        // 不查 `SyllableTrie`：`syl.starts_with(t)` 向候选自己的真值音节求证，更直接。
        let two = (allow_two && pos + 2 <= raw.len()).then(|| &raw[pos..pos + 2]);
        let seg = if raw[pos..].starts_with(syl) {
            *syl
        } else if let Some(t) = two
            && t.as_bytes()[1] == b'h'
            && syl.starts_with(t)
        {
            t
        } else {
            let c = &raw[pos..pos + 1];
            if !syl.starts_with(c) {
                return None; // 连声母都对不上：这个候选不是这串击键打出来的
            }
            c
        };
        out.push_str(seg);
        pos += seg.len();
    }
    Some((out, pos))
}

#[cfg(test)]
#[path = "mixed_abbrev_tests.rs"]
mod tests;
