//! 混合简拼：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
//!
//! 设计文档：`docs/design/pinyin-mixed-abbrev.md`。
//!
//! ## 为什么这里只产「模式」，不产「切分」
//!
//! 文档 §4 的三个方案里，A（给 `Dag` 加声母节点）要动逐键候选生成的热路径且路径数膨胀，
//! B（把 `nhao` 展开成 `nihao`/`nuhao`/… 逐个查）是**猜**，C（索引另存前缀简拼）要 bump
//! wdat 且只解决一半。本模块走第四条路：
//!
//! ```text
//!   nhao ──切分──> [Initial('n'), Syllable("hao")]   ← 模式（本模块）
//!        ──投影──> "nh"                              ← 声母串，正是 AbbrevSection 现有的键
//!        ──点查──> "nihao"（真值全拼码，非推断）
//!        ──校验──> ni|hao 逐段比对：n? ✓ / ==hao ✓   ← 模式在这里第二次发挥作用
//! ```
//!
//! 投影键退化成纯简拼，所以**索引一个字节都不用改**；混合信息全部留在模式里做后置校验，
//! 所以不会像纯简拼那样把 `nh` 下的词一股脑捞出来。`nih`（全拼在前）与 `nhao`（声母在前）
//! 是同一套模式的两个实例，文档 §2 说的「卡在不同环节」在这里合成了一处。
//!
//! ## 与纯简拼的分工
//!
//! 本模块认**既有声母段又有音节段**的解释，外加**含双字母声母**的那些：
//! - 全是单字母声母段（`nh`）→ 纯简拼，由 `AbbrevMatcher` + step5 处理，这里返回空；
//! - 全是音节段（`nihao`）→ 全拼，走主路径；
//! - 含 `zh`/`ch`/`sh` 段（`zhy` = zh|y）→ **归本模块，即使全是声母段**。纯简拼路径
//!   逐字母切，只能把 `zhy` 解释成 z|h|y、投影键 `zhy`，而「这样」挂在键 `zy` 下 ——
//!   那条路径表达不了「zh 是一个声母」，所以这里必须接住。
//!
//! 调用方还应先确认整串**不能**被完整切成音节序列，否则常见全拼输入会白跑一趟（见
//! `PinyinEngine::convert` step 5b 的短路）。

use super::syllable::SyllableTrie;

/// 模式最大段数。与 `AbbrevMatcher::find_candidates` 的简拼上限（6）一致——
/// 再长的词打混合简拼已无收益，而段数直接决定 DFS 深度。
const MAX_SEGMENTS: usize = 6;

/// 单串最多保留的模式数。达到上限即停止枚举（DFS 顺序固定，故截断是确定性的：
/// **长音节优先于短音节、音节段优先于双字母声母、双字母声母优先于单字母声母**，
/// 即更具体的解释先被保留）。
///
/// ★ **从 16 提到 24 是因为双字母声母加了一档**：`zh`/`ch`/`sh` 的位置上多出一条边，
/// 深层位置的分支互相挤，`chengshizhong` 实测丢掉了 3 条**改动前就有**的解释
/// （键 `chesz`/`cheszh`/`chegsz`）。新增一个维度却不给额度，等于拿新解释换旧解释。
/// 24 的依据是实测：该串新旧解释合计 16 条，`shanghaishizh` 等其余样本更少，
/// 留出余量后仍远低于「每条模式一次索引点查」的成本拐点（见
/// `tests/pinyin_abbrev_recall_latency.rs` 的真机计时）。
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
    /// 它不是 [`Initial`](Self::Initial) 的特例而是独立一段，因为二者**消耗的击键数不同**
    /// （2 vs 1）：`walk` 原先靠「边长 == 1」判声母段，混进 2 字节的声母后那条判据必错
    /// 且不会报错（见 [`Edge`]）。
    Retroflex(char),
    /// 音节段：约束对应音节**全等**。
    Syllable(String),
}

impl AbbrevSeg {
    /// 本段与词典音节的**精确**匹配判定 —— 段语义的唯一真相源。
    ///
    /// 模糊放宽由调用方在此基础上叠加（见 `PinyinEngine::seg_matches_fuzzy`）：先问这里，
    /// 不中再试变体。把 `starts_with` / 全等这两条留在本模块，是为了让「声母段只约束
    /// 首字母、音节段约束全等」这个定义只有一处。
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
    /// **音节数相等是硬条件**，这继承自纯简拼那条「字母数 == `boundary.count_ones()`」的
    /// 过滤（文档 §5 约束 3）：扁平码有损，`xian` 既是「西安」的 xi|an 也是「先」的 xian，
    /// 不按音节数卡住就会捞出一串权重高得多的单字。混合形态下的口径即**段数**。
    /// ⚠️ **生产代码已无调用点**，四处段校验全部走 [`Self::matches_with`]（要拿模糊处数
    /// 施加折扣）。保留它是作为「精确比较」的语义锚点与本模块单测的入口；若哪天单测也不用了，
    /// 直接删掉即可，不要为了「保持对称」再给它接回生产路径。
    pub fn matches<S: AsRef<str>>(&self, syllables: &[S]) -> bool {
        self.matches_with(syllables, |seg, syl| seg.matches_exact(syl).then_some(0))
            .is_some()
    }

    /// 同 [`Self::matches`]，但音节段的比较交给调用方 —— 供模糊音放宽。
    ///
    /// 闭包对**每一段**（声母段与音节段都算）返回 `Some(edits)`（该段的模糊改动处数，
    /// `0` = 精确匹配）或 `None`（不匹配）；
    /// 整条模式的返回值是各段 `edits` 之和。**调用方必须据此施加
    /// [`fuzzy_penalized`](crate::pinyin) 折扣并标 `is_fuzzy`** —— 模糊命中恒低精确命中一档
    /// 是全仓不变量（见 `FUZZY_WEIGHT_SCALE` 那段论证），本路径不是例外。
    /// 返回处数而非 `bool`，正是为了让这个不变量在类型上没法被忽略。
    ///
    /// **只有 `Syllable` 段需要这个钩子**：`Initial` 段比的是首字母，而模糊音的声母组
    /// （`sh↔s`、`zh↔z`、`ch↔c`）恰好共享首字母，`starts_with` 天然就是宽松的
    /// —— 用户敲 `s`，词典里的 `sheng` 本来就匹配得上。
    ///
    /// 真机现场：`senrikl` 想要「生日快乐」。声母投影键两边都是 `srkl`，
    /// `search_abbrev` 已经把 `shengrikuaile` 召回来了，却在这里被
    /// `"sen" == "sheng"` 判否丢弃 —— 模糊音在召回侧生效、在校验侧不生效，
    /// 于是整条路白走。判据的方向必须与 `lookup_with_fuzzy::expand_code` 一致：
    /// 对**用户输入段**做扩展，去匹配**词典音节**。
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
/// **必须带类型而不能只给长度**：`zh` 与 `ba` 都是 2 字节，前者是声母段、后者是音节段，
/// 靠长度区分不了。此前 `walk` 用 `len == 1` 判「是不是声母段」，双字母声母进来后
/// 那条判据就失效了。
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
    let syls = trie.match_at(input, pos).into_iter().filter_map(|s| {
        let n = s.len();
        (n > 1).then_some(Edge::Syllable(n))
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
/// 判据仍向 trie 求证（`trie.is_prefix("zh")`）而非硬编码三个字面量：音节表是这件事的
/// 真相源，写死一份就多一处会漂移的副本。
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
        // ★ 含 `Retroflex` 段的模式**即使全是声母段也要收**：`zhy` = zh|y 是纯简拼路径
        // 表达不了的形态（它逐字母切，只能给出 z|h|y、投影键 `zhy`），而「这样」挂在
        // 键 `zy` 下。不为它放行，双字母声母就只在「混着全拼音节打」时有效，
        // 恰恰漏掉了用户最常写的那种（`zhy`/`zhsh`）。
        let has_retroflex = cur.iter().any(|s| matches!(s, AbbrevSeg::Retroflex(_)));
        let has_initial = cur
            .iter()
            .any(|s| matches!(s, AbbrevSeg::Initial(_) | AbbrevSeg::Retroflex(_)));
        let has_syllable = cur.iter().any(|s| matches!(s, AbbrevSeg::Syllable(_)));
        // ⚠️ **投影键至少 2 位**（= 段数 ≥ 2）。`Retroflex` 吃 2 个击键却只投影 1 个字母，
        // 单段模式（`zh` → 键 `z`）于是绕过了 `MIN_ABBREV_STROKE` 立的规矩：
        // 「单字母不构成简拼，退到 1 只会拖出一堆高频单字」。实测 `zhq` 会在 step 6.2 的
        // `zh` 切点上把键 `z` 下所有 zh 开头的**单音节**词（这/中/只）连同单字的巨大权重
        // 灌进候选，而它们只解释了 3 键里的 2 键。本次要修的三条（zhy/zhge/baichx）
        // 都是 ≥2 段，不受此闸影响。
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
/// 这是混合校验唯一的音节来源：模式比对的是「第 k 个音节长什么样」，没有真值切分就没有
/// 判据。故 `boundary == 0`（旧数据 / 用户手输码 / 五笔码）一律返回 `None` **不参与**混合
/// 简拼——注意这与全仓「任一侧为 0 即降级放行」的惯例方向相反，因为那条惯例讲的是
/// 「校验放宽」，而这里 boundary 缺失等于判据本身不存在，放行就成了不校验。
///
/// ⚠️ 与 [`super::PinyinEngine::abbrev_of_code`] **必须对同一个 boundary 给出一致的解释**
/// （那边取的正是这里每段的首字母）。改动其一时同步核对另一处。
pub fn syllables_from_boundary(code: &str, boundary: u64) -> Option<Vec<&str>> {
    // bit0 未置位 = 第一个音节不从 0 开始 —— 坏数据，不猜。
    //
    // `!code.is_ascii()`：下面按**字节**下标切片，`i` 落在多字节字符内部会 panic。
    // 同文件的 `render_keystroke_preedit` 早就带着同款守卫，说明本模块不把 ASCII 当
    // 可假设的前提。此前本函数的调用方都带 `is_abbrev` 一类的窄化守卫，`shuangpin_code_of`
    // 把调用面扩到了**每条拼音候选**、且处在按键线程持 state 锁的位置 —— 那里 panic
    // 就是整个输入法崩掉，而这行的成本是零。
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
/// ## 为什么不能直接渲染候选的 code
///
/// preedit 必须与击键串**同域**。简拼/混合简拼候选的 code 是词的全拼码（`nihao`），
/// 拿它走 [`super::render_preedit`] 会显示成 `ni'hao` —— 用户只敲了 4 键却看到 5 个字母，
/// 退格与光标编辑立刻错位。要显示的是「用户敲的这几个键怎么分段」，不是「这个词怎么拼」。
///
/// ## 切法
///
/// 逐音节贪心：当前位置能整段对上该音节就是**音节段**（吃掉整个音节），对不上就只能是
/// **声母段**（吃 1 字节，或 `zh`/`ch`/`sh` 那 2 字节，且该声母必须是这个音节的开头）。
/// 音节段优先是对的 —— 声母段是「信息更少」的解释，只在整段对不上时才成立。
///
/// ## 为什么双字母声母要**两遍**
///
/// 贪心无回溯，而 `zh` 这一位有两种都合法的读法，且哪种对**取决于后面走不走得通**：
///
/// | 击键 | 候选音节 | 正确切法 |
/// |---|---|---|
/// | `zhge` | zhe\|ge | `zh'ge` —— zh 是一个声母段 |
/// | `zh`   | zhong\|hua | `z'h` —— 老写法，z 和 h 各是一段 |
///
/// 只按「双字母优先」单遍扫，第二行会在第一步吃掉 2 字节、第二个音节没键可分，
/// 整个函数返回 `None`，组合区退回无分隔符的 `zh`。那正是本模块要显示给用户的东西
/// （`z'h'ge` 这个显示曾是双字母声母缺失的**唯一**可见线索），不能因为新增了一种解释
/// 就把另一种的显示弄丢 —— 候选侧两种解释是并存的，显示侧也必须并存。
///
/// 故先按双字母优先试一遍，不成再按单字母试一遍。两遍都失败才返回 `None`。
/// 不会出现「两遍都成功但结果不同」的歧义：能整串对齐的切法对给定音节序列是唯一的
/// （每一步的候选段互不为前缀）。
///
/// 返回 `(渲染串, 已消费的 raw 字节数)`。**部分匹配**（step 6.2 前缀回退）时消费数会小于
/// `raw.len()`，余下的字母由调用方自己切分后追加——尾巴往往还含完整音节
/// （`bzdnihaob` 的 `haob` = `hao` + 残码 `b`），整段甩上去会显示成 `b'z'd'ni'haob`，
/// 该切的地方没切。
///
/// 任何一步对不上一律返回 `None`（调用方保持原显示不变）。preedit 是显示层，宁可少一个
/// 分隔符，不可给出与击键长度不符的串——**去掉 `'` 必须恰好还原击键串**是这里的不变量。
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
        // ⚠️ 这里刻意**不查 `SyllableTrie`**（与 `is_retroflex_at` 不同，那边查是为了不
        // 硬编码三个字面量）：判据 `syl.starts_with(t)` 是向**候选自己的真值音节**求证，
        // 比查音节表更直接，也免去把 trie 穿进显示层。第二位是 `h` 那一条只是先筛掉
        // 绝大多数不可能的位置。
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
mod tests {
    use super::*;

    fn trie() -> SyllableTrie {
        SyllableTrie::new()
    }

    fn keys(input: &str) -> Vec<String> {
        let mut k: Vec<String> = mixed_patterns(input, &trie())
            .iter()
            .map(|p| p.key().to_string())
            .collect();
        k.sort();
        k.dedup();
        k
    }

    /// 声母在前：`nhao` 的主解释是 [n][hao]，投影键退化为纯简拼 `nh`。
    /// **`nh` 正是 AbbrevSection 里已有的键**——整个方案成立与否就在这一行。
    #[test]
    fn initial_first_projects_to_plain_abbrev_key() {
        let pats = mixed_patterns("nhao", &trie());
        let main = pats
            .iter()
            .find(|p| p.len() == 2)
            .expect("nhao 应有 [n][hao] 这条 2 段解释");
        assert_eq!(main.key(), "nh");
        assert!(main.matches(&["ni", "hao"]), "ni|hao 应命中");
        assert!(main.matches(&["na", "hao"]), "声母段只约束首字母");
        assert!(!main.matches(&["ni", "hai"]), "音节段要求全等");
        assert!(!main.matches(&["ni", "hao", "ma"]), "音节数必须相等");
    }

    /// 全拼在前：`nih` 在 `is_abbreviation` 那里连门都进不去（`i` 不是任何音节首字母），
    /// 但作为模式它完全成立 —— 文档 §2 说的「两种形态卡在不同环节」在这里被同一套表示统一。
    #[test]
    fn syllable_first_is_expressible() {
        let pats = mixed_patterns("nih", &trie());
        let main = pats
            .iter()
            .find(|p| p.matches(&["ni", "hao"]))
            .expect("nih 应能匹配 ni|hao");
        assert_eq!(main.key(), "nh", "投影键同样退化为纯简拼");
        assert!(!main.matches(&["ni", "ao"]), "第二段须以 h 开头");
    }

    /// 全声母串不归本模块 —— 那是纯简拼，由 `AbbrevMatcher` + step5 处理，
    /// 在这里产出只会与之重复召回。
    #[test]
    fn pure_initial_form_is_excluded() {
        assert!(keys("nh").is_empty());
        assert!(keys("dblg").is_empty());
    }

    /// **完整全拼串照样有混合解释**（`nihao` = ni + h + ao），本模块不负责挡它 ——
    /// 挡它的是引擎 step 5b 的 `mixed_covered` 短路。
    ///
    /// 这条断言把职责分工钉死：短路不是性能优化，而是正确性依赖。哪天它被改掉，
    /// 常见全拼输入就会静默多出一批 is_abbrev 层的噪音候选，且毫无痕迹。
    #[test]
    fn full_pinyin_still_has_mixed_readings_caller_must_short_circuit() {
        assert!(
            !keys("nihao").is_empty(),
            "ni|h|ao 是合法混合式；排除它是调用方的职责，不是枚举器的"
        );
        assert!(!keys("xian").is_empty(), "xi|a|n 同理");
    }

    /// 判据①失败的那批串（`woain`）在这里是合法混合式 —— 恰好就是用户想要的「我爱你」。
    #[test]
    fn partial_syllable_tail_is_mixed() {
        let pats = mixed_patterns("woain", &trie());
        assert!(
            pats.iter().any(|p| p.matches(&["wo", "ai", "ni"])),
            "wo|ai|n 应能匹配 wo|ai|ni: {:?}",
            pats.iter().map(|p| p.key()).collect::<Vec<_>>()
        );
    }

    /// 单字母音节（`a`/`e`/`o`）不重复产出「音节段 + 声母段」两条同长边。
    /// 声母段的约束是音节段的超集，留两条只是把同一批词查两遍。
    #[test]
    fn single_letter_syllable_does_not_duplicate_initial_edge() {
        let pats = mixed_patterns("hao", &trie());
        // h + ao / ha + o(声母) —— 后者的 o 只能是声母段，不该同时再出一条音节段 [o]
        let with_o_tail: Vec<_> = pats.iter().filter(|p| p.len() == 2).collect();
        let dup = with_o_tail
            .iter()
            .filter(|p| p.matches(&["ha", "o"]))
            .count();
        assert!(dup <= 1, "「ha|o」不应有两条等价模式: {with_o_tail:?}");
    }

    /// 非法输入一律不解释（含大写/数字/分隔符——分隔符是全拼的硬边界，与简拼无关）。
    #[test]
    fn rejects_non_lowercase_and_extremes() {
        assert!(keys("NHao").is_empty());
        assert!(keys("n2ao").is_empty());
        assert!(keys("ni'h").is_empty());
        assert!(keys("n").is_empty(), "单字母不构成混合式");
        assert!(
            keys(&"nhao".repeat(5)).is_empty(),
            "超长串不做混合解释（20 字节 > 上限）"
        );
    }

    /// 模式数有硬上限，且枚举不得随长度爆炸 —— 这是热路径上的成本闸门。
    #[test]
    fn pattern_count_is_bounded() {
        for input in [
            "zhongguorenm",
            "nhaoshijien",
            "wdjdxzgr",
            "aeiouaeiou",
            // 卷舌串：双字母声母加了一档边，分支最密的形状要一并看住。
            "chengshizhong",
            "shanghaishizh",
            "zhchshzh",
            "zhzhzhzh",
        ] {
            let pats = mixed_patterns(input, &trie());
            assert!(
                pats.len() <= MAX_PATTERNS,
                "{input}: {} 条超过上限",
                pats.len()
            );
            assert!(pats.iter().all(|p| p.len() <= MAX_SEGMENTS));
        }
    }

    /// **新增一个维度不得挤掉旧解释**。
    ///
    /// `Retroflex` 边排在 `Initial` 边之前，同一位置上新分支先展开，深层位置会把
    /// `MAX_PATTERNS` 的额度提前用光。`chengshizhong` 实测曾因此丢掉三条改动前就有的
    /// 解释 —— 那不是「截断」而是「换掉」，用户感知为「某些老写法忽然打不出词了」。
    #[test]
    fn retroflex_edges_do_not_evict_pre_existing_interpretations() {
        let k = keys("chengshizhong");
        for expected in ["chesz", "cheszh", "chegsz"] {
            assert!(
                k.contains(&expected.to_string()),
                "丢了改动前就有的解释 {expected}：{k:?}"
            );
        }
    }

    /// 投影键至少 2 位：`Retroflex` 吃 2 个击键却只投影 1 个字母，单段模式会绕过
    /// `MIN_ABBREV_STROKE`（「单字母不构成简拼，退到 1 只会拖出一堆高频单字」）。
    #[test]
    fn single_retroflex_segment_does_not_yield_one_letter_key() {
        assert!(keys("zh").is_empty(), "zh 单独一段投影成键 `z`，不该产出");
        assert!(keys("ch").is_empty());
        assert!(keys("sh").is_empty());
        // 两段起才算数。
        assert_eq!(keys("zhy"), vec!["zy".to_string()]);
    }

    /// 双字母声母的模式与投影键。
    #[test]
    fn retroflex_patterns_project_to_existing_keys() {
        assert!(keys("zhge").contains(&"zg".to_string()), "zh|ge → zg");
        assert!(
            keys("baichx").contains(&"bcx".to_string()),
            "bai|ch|x → bcx"
        );
        // 老解释并存：zhge 同时还有 z|h|ge。
        assert!(
            keys("zhge").contains(&"zhg".to_string()),
            "z|h|ge → zhg 仍在"
        );
    }

    /// 段语义：`Retroflex` 要求音节以两个字母开头，比 `Initial` 严。
    #[test]
    fn retroflex_segment_is_stricter_than_initial() {
        assert!(AbbrevSeg::Retroflex('z').matches_exact("zhe"));
        assert!(!AbbrevSeg::Retroflex('z').matches_exact("ze"));
        assert!(!AbbrevSeg::Retroflex('z').matches_exact("z"));
        assert!(AbbrevSeg::Initial('z').matches_exact("ze"), "单字母仍宽松");
    }

    /// preedit 渲染：两种解释的显示都要在。
    ///
    /// 单遍「双字母优先」贪心会让第二行返回 `None`（第一步吃掉 2 字节、第二个音节没键可分），
    /// 组合区退回无分隔符的 `zh` —— 而 `z'h'ge` 这种显示正是双字母声母缺失时用户能看到的
    /// 唯一线索，不能因为新增一种解释就把另一种的显示弄丢。
    #[test]
    fn preedit_renders_both_retroflex_and_single_letter_splits() {
        assert_eq!(
            render_keystroke_preedit("zhge", &["zhe", "ge"]),
            Some(("zh'ge".into(), 4)),
            "zh 是一个声母段"
        );
        assert_eq!(
            render_keystroke_preedit("zh", &["zhong", "hua"]),
            Some(("z'h".into(), 2)),
            "老写法：z 和 h 各是一段"
        );
        assert_eq!(
            render_keystroke_preedit("baichx", &["bai", "cheng", "xian"]),
            Some(("bai'ch'x".into(), 6))
        );
        // 不变量：去掉 ' 恰好还原击键串的已消费部分。
        for (raw, syls) in [
            ("zhge", &["zhe", "ge"][..]),
            ("zh", &["zhong", "hua"][..]),
            ("baichx", &["bai", "cheng", "xian"][..]),
        ] {
            let (rendered, used) = render_keystroke_preedit(raw, syls).expect("应渲染得出");
            assert_eq!(rendered.replace('\'', ""), raw[..used]);
        }
    }

    /// boundary 切分与 `abbrev_of_code` 同源：每段首字母拼起来必须等于纯简拼串。
    #[test]
    fn boundary_split_agrees_with_abbrev_projection() {
        // 「西安宁」xi|an|ning：位 0/2/4
        let syls = syllables_from_boundary("xianning", 0b10101).expect("有边界");
        assert_eq!(syls, vec!["xi", "an", "ning"]);
        let abbrev: String = syls.iter().filter_map(|s| s.chars().next()).collect();
        assert_eq!(abbrev, "xan", "与 abbrev_of_code 的投影必须一致");

        assert_eq!(
            syllables_from_boundary("nihao", 0b101),
            Some(vec!["ni", "hao"])
        );
        assert_eq!(syllables_from_boundary("xian", 0b1), Some(vec!["xian"]));
    }

    /// 无边界信息 = 判据不存在 → 不参与混合简拼（**不是**放行）。
    #[test]
    fn missing_boundary_yields_no_syllables() {
        assert_eq!(syllables_from_boundary("nihao", 0), None);
        // bit0 未置位：第一个音节不从 0 开始，坏数据，不猜
        assert_eq!(syllables_from_boundary("nihao", 0b100), None);
        assert_eq!(syllables_from_boundary("", 0b1), None);
    }
}
