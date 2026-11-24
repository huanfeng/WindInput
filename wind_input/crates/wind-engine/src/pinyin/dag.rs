// Portions derived from librime / librime-octagram, Copyright (c) 2014 RIME Developers, BSD-3-Clause (see NOTICE.md).
//! DAG 构建与最大匹配
//!
//! DP 最大匹配切分拼音音节。

use crate::pinyin::syllable::SyllableTrie;

/// DAG 节点
#[derive(Debug, Clone)]
pub struct DagNode {
    pub start: usize,
    pub end: usize,
    pub syllable: String,
    /// 换位纠错拼写的边（[`SyllableTrie::is_correction_spelling`]）。
    pub corrected: bool,
}

/// 有向无环图
pub struct Dag {
    /// nodes[i] = 从位置 i 出发的所有边
    nodes: Vec<Vec<DagNode>>,
    input: String,
}

impl Dag {
    /// 构建 DAG：对每个位置匹配所有可能的音节。
    ///
    /// 含 trie 的模糊拼写层（若已注册），使 `tinzhi` 这类错音串切得动 —— 模糊音的展开
    /// 发生在切分**之后**，切不出来就等于整条模糊链路没被执行。
    /// 真值推导请改用 [`Self::build_strict`]。
    pub fn build(input: &str, trie: &SyllableTrie) -> Self {
        Self::build_inner(input, trie, false)
    }

    /// 只按标准音节表切分。见 [`SyllableTrie::match_at_strict`]。
    pub fn build_strict(input: &str, trie: &SyllableTrie) -> Self {
        Self::build_inner(input, trie, true)
    }

    fn build_inner(input: &str, trie: &SyllableTrie, strict: bool) -> Self {
        let n = input.len();
        let mut nodes = vec![Vec::new(); n];

        for (i, slot) in nodes.iter_mut().enumerate() {
            let matches = if strict {
                trie.match_at_strict(input, i)
            } else {
                trie.match_at(input, i)
            };
            for syl in matches {
                let end = i + syl.len();
                slot.push(DagNode {
                    start: i,
                    end,
                    corrected: trie.is_correction_spelling(&syl),
                    syllable: syl,
                });
            }
        }

        Self {
            nodes,
            input: input.to_string(),
        }
    }

    /// DP 最大匹配（非贪心，覆盖最多字符）
    ///
    /// 为什么不用贪心： "henihejiele" 贪心选 "hen" 后 "i" 无法匹配。
    /// DP 选 "he"+"ni"+"he"+"jie"+"le" 覆盖全部。
    ///
    /// **不走换位纠错边**（`corrected`）：主切分决定编码栏、子短语、简拼闸门等整条下游，
    /// 纠错边一旦当选（`yuo` 同覆盖时长边先到、`xign` 覆盖更多）就把关纠错时的
    /// `yu|o`、`xi|gn` 解释整批换掉。纠错只能加候选：纠错词另由 [`Self::maximum_match_with_correction`]
    /// 那条切分单独查（`PinyinEngine` 的纠错召回步），不经主切分。
    pub fn maximum_match(&self) -> Vec<String> {
        self.maximum_match_inner(false)
    }

    /// 同 [`Self::maximum_match`]，但走换位纠错边（`gogn` → `[gogn]`）。
    pub fn maximum_match_with_correction(&self) -> Vec<String> {
        self.maximum_match_inner(true)
    }

    fn maximum_match_inner(&self, with_corrected: bool) -> Vec<String> {
        let n = self.input.len();
        if n == 0 {
            return Vec::new();
        }

        // dp[i] = 位置 i 之前最多覆盖的字符数，-1 表示不可达
        let mut dp = vec![-1i32; n + 1];
        dp[0] = 0;

        // prev[i] = 到达位置 i 的最优路径中，最后一个音节
        let mut prev_syl = vec![String::new(); n + 1];
        let mut prev_pos = vec![0usize; n + 1];

        for pos in 0..n {
            if dp[pos] < 0 {
                continue;
            }
            for node in self.nodes[pos]
                .iter()
                .filter(|n| with_corrected || !n.corrected)
            {
                let end = node.end;
                let covered = dp[pos] + (end - pos) as i32;
                if covered > dp[end] {
                    dp[end] = covered;
                    prev_syl[end] = node.syllable.clone();
                    prev_pos[end] = pos;
                }
            }
        }

        // 从最远可达位置回溯
        let mut best_end = 0;
        for i in (0..=n).rev() {
            if dp[i] >= 0 {
                best_end = i;
                break;
            }
        }

        let mut result = Vec::new();
        let mut pos = best_end;
        while pos > 0 {
            let syl = prev_syl[pos].clone();
            if syl.is_empty() {
                break;
            }
            result.push(syl);
            pos = prev_pos[pos];
        }

        result.reverse();
        result
    }

    /// 获取未匹配的尾部（从最远可达位置到输入末尾）
    pub fn unmatched_tail(&self) -> &str {
        let n = self.input.len();
        if n == 0 {
            return "";
        }

        let mut dp = vec![-1i32; n + 1];
        dp[0] = 0;

        for pos in 0..n {
            if dp[pos] < 0 {
                continue;
            }
            for node in &self.nodes[pos] {
                let covered = dp[pos] + (node.end - pos) as i32;
                if covered > dp[node.end] {
                    dp[node.end] = covered;
                }
            }
        }

        // 找到最远可达位置（从后往前找第一个可达点，命中即停）
        let best = dp.iter().rposition(|&v| v >= 0).unwrap_or(0);

        &self.input[best..]
    }

    /// 获取从指定位置开始的所有可能音节
    pub fn edges_from(&self, pos: usize) -> &[DagNode] {
        if pos < self.nodes.len() {
            &self.nodes[pos]
        } else {
            &[]
        }
    }

    /// 输入长度
    pub fn input_len(&self) -> usize {
        self.input.len()
    }

    /// 是否有从指定位置出发的边
    pub fn has_edges_from(&self, pos: usize) -> bool {
        pos < self.nodes.len() && !self.nodes[pos].is_empty()
    }
}

/// 切分图：把「从字节位置 p 出发有哪些音节」这一事实单独抽出来。
///
/// 词图构建的两种切分来源统一成「图」：
///
/// - 全拼：多路径图（`Dag` 的全部边）。只取 `maximum_match` 的一条会丢掉真值路径
///   （「西安」`xi|an`），边界校验随之把词逐出词图。
/// - 双拼 / 手动分隔符 `'`：切分是真值，退化为线性链，绝不可让 DAG 重猜
///   （双拼 `nihao` 是 `ni|ha|o`，重猜成 `ni|hao` 会让 5 键也能出「你好」）。
///
/// 边只存终点位置：音节本身恒为 `input[p..q]`，无须重复存储。
pub struct SegGraph {
    /// edges[p] = 从字节位置 p 出发的音节终点（升序）
    edges: Vec<Vec<usize>>,
    /// 从 0 可达的位置
    reachable: Vec<bool>,
    /// ambiguous[j] = 从 j 出发、且**处在歧义接缝上**的音节终点（升序）。
    ///
    /// 判据（照搬 librime `Syllabifier::CheckOverlappedSpellings`，
    /// `ref/weasel/librime/src/rime/algo/syllabifier.cc:243-276`）：
    /// 若存在 p 使得 `p→j`、`j→q`、`p→q` 三条边同时成立（即整段 `Z` 又能拆成 `Y+X`），
    /// 则 j 是歧义接缝，**后半段** `j→q` 被标记。
    ///
    /// 例：`lian` = `li`+`an` → 边 `an`(2→4) 歧义；`hua` = `hu`+`a` → 边 `a` 歧义。
    /// 这正是 A 类 13 条回归的全部形态（`ye|xi|an`、`guo|ti|an`、`hu|a|long`）。
    ambiguous: Vec<Vec<usize>>,
    len: usize,
}

/// `mask_path` 的三态结果。
pub enum MaskCheck {
    /// mask 是 p→q 的一条合法路径，携带其音节数
    Path(usize),
    /// 无边界信息（mask==0：五笔码 / code 超 64 字节 / 旧格式）→ 降级放行
    NoInfo,
    /// mask 与本跨度的任何合法切分都不符 → 该词不是用户按这个切分敲出来的
    Reject,
}

impl SegGraph {
    fn finish(edges: Vec<Vec<usize>>, len: usize) -> Self {
        let mut reachable = vec![false; len + 1];
        reachable[0] = true;
        for p in 0..=len {
            if !reachable[p] {
                continue;
            }
            if let Some(es) = edges.get(p) {
                for &q in es {
                    reachable[q] = true;
                }
            }
        }
        // 歧义接缝普查：三重循环但每层度数极小（一个位置至多几条音节边），远小于词典查询开销。
        let mut ambiguous: Vec<Vec<usize>> = vec![Vec::new(); len + 1];
        for p in 0..=len {
            let Some(from_p) = edges.get(p) else { continue };
            for &j in from_p {
                let Some(from_j) = edges.get(j) else { continue };
                for &q in from_j {
                    // p→q 也是一条边 ⇒ 整段 `Z` 又能拆成 `Y+X` ⇒ j 是歧义接缝
                    if from_p.binary_search(&q).is_ok() && !ambiguous[j].contains(&q) {
                        ambiguous[j].push(q);
                    }
                }
            }
        }
        for v in ambiguous.iter_mut() {
            v.sort_unstable();
        }
        Self {
            edges,
            reachable,
            ambiguous,
            len,
        }
    }

    /// 全拼：消费 DAG 的**全部**切分路径，但**不收换位纠错边**（`DagNode::corrected`）。
    ///
    /// 纠错只能加候选。纠错边进了这张图，整句（只出一条）、模糊变体、简拼就都可能经它
    /// 抢先：整句「哟呢」换成「咏鹅」（`yone`），`boa` 的「薄」先以纠错身份占了同文位、
    /// 再在截断时让位而整条消失。纠错词只由纠错召回步经 [`Self::from_dag_with_correction`] 查。
    pub fn from_dag(dag: &Dag) -> Self {
        Self::from_dag_filtered(dag, false)
    }

    /// 同 [`Self::from_dag`]，含换位纠错边。只给纠错召回用。
    pub fn from_dag_with_correction(dag: &Dag) -> Self {
        Self::from_dag_filtered(dag, true)
    }

    fn from_dag_filtered(dag: &Dag, with_corrected: bool) -> Self {
        let len = dag.input_len();
        let mut edges = vec![Vec::new(); len + 1];
        for (p, slot) in edges.iter_mut().take(len).enumerate() {
            let mut ends: Vec<usize> = dag
                .edges_from(p)
                .iter()
                .filter(|n| with_corrected || !n.corrected)
                .map(|n| n.end)
                .collect();
            ends.sort_unstable();
            ends.dedup();
            *slot = ends;
        }
        Self::finish(edges, len)
    }

    /// 双拼 / 手动分隔符：切分是真值，图退化为一条线性链。
    pub fn from_syllables(syllables: &[String]) -> Self {
        let len: usize = syllables.iter().map(|s| s.len()).sum();
        let mut edges = vec![Vec::new(); len + 1];
        let mut pos = 0usize;
        for s in syllables {
            if s.is_empty() {
                continue;
            }
            edges[pos].push(pos + s.len());
            pos += s.len();
        }
        Self::finish(edges, len)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn edges_from(&self, pos: usize) -> &[usize] {
        self.edges.get(pos).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 位置 p 是否从 0 可达。不可达的位置上建节点纯属浪费——Viterbi 永远到不了那里。
    pub fn is_reachable(&self, pos: usize) -> bool {
        self.reachable.get(pos).copied().unwrap_or(false)
    }

    fn has_edge(&self, p: usize, q: usize) -> bool {
        self.edges_from(p).binary_search(&q).is_ok()
    }

    /// 音节边 `p→q` 是否处在歧义接缝上（见 `ambiguous` 字段）。
    pub fn is_ambiguous_edge(&self, p: usize, q: usize) -> bool {
        self.ambiguous
            .get(p)
            .map(|v| v.binary_search(&q).is_ok())
            .unwrap_or(false)
    }

    /// 一条切分（各音节起点相对 `p` 的偏移，跨度 `p..q`）中处在歧义接缝上的音节数。
    pub fn ambiguous_count(&self, p: usize, q: usize, offsets: &[usize]) -> usize {
        let mut n = 0;
        for (i, &o) in offsets.iter().enumerate() {
            let s = p + o;
            let e = offsets.get(i + 1).map(|&x| p + x).unwrap_or(q);
            if self.is_ambiguous_edge(s, e) {
                n += 1;
            }
        }
        n
    }

    /// 从 p 出发、经 1..=`max_edges` 条边可达的全部终点（升序去重）。
    ///
    /// 这是词图查询的**枚举面**：`(p, q)` 对唯一决定查询码 `input[p..q]`
    /// （音节恒为输入的连续子串，故 `syllables[start..end].join("")` 恒等于 `input[p..q]`）。
    /// **不枚举路径**——路径条数可指数增长，而跨度对至多 O(n²)。
    pub fn ends_within(&self, p: usize, max_edges: usize) -> Vec<usize> {
        let mut seen = vec![false; self.len + 1];
        let mut out: Vec<usize> = Vec::new();
        let mut frontier: Vec<usize> = vec![p];
        for _ in 0..max_edges {
            let mut next: Vec<usize> = Vec::new();
            for &cur in &frontier {
                for &q in self.edges_from(cur) {
                    if !seen[q] {
                        seen[q] = true;
                        out.push(q);
                        next.push(q);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        out.sort_unstable();
        out
    }

    /// 词典给的边界 `mask`（code 内各音节起始字节位）是否恰是 p→q 的一条合法路径。
    ///
    /// **这是本次改造的关键手法**：不去枚举路径再比对，而是把词条自带的边界**当作一条
    /// 待验证的路径**逐段查图。代价 O(音节数)，与图中路径总数无关——路径爆炸因此
    /// 在结构上不可能发生。
    ///
    /// 判据同时兼作 max_word_len 闸门：返回的音节数由调用方比对上限。
    pub fn mask_path(&self, p: usize, q: usize, mask: u64) -> MaskCheck {
        if mask == 0 {
            return MaskCheck::NoInfo; // 无信息 → 不设防（与全仓「boundary==0 降级放行」一致）
        }
        let l = q.saturating_sub(p);
        if l == 0 || l > 64 {
            return MaskCheck::Reject;
        }
        if mask & 1 == 0 {
            return MaskCheck::Reject; // 首音节必起于 code 起点
        }
        // 越出 code 范围的位 → 这份 mask 描述的不是本跨度
        if l < 64 && (mask >> l) != 0 {
            return MaskCheck::Reject;
        }
        let mut cur = 0usize;
        let mut count = 0usize;
        while cur < l {
            let mut nxt = cur + 1;
            while nxt < l && (mask >> nxt) & 1 == 0 {
                nxt += 1;
            }
            if !self.has_edge(p + cur, p + nxt) {
                return MaskCheck::Reject; // 该段不是合法音节 → 用户敲不出这个切分
            }
            cur = nxt;
            count += 1;
        }
        MaskCheck::Path(count)
    }

    /// 任取一条 p→q 的路径（边数 ≤ `max_edges`），返回各音节的**起点偏移**（相对 p）。
    /// 供无边界信息（降级放行）与模糊变体命中使用——它们没有可信的真值切分，
    /// 但节点仍需一个自洽的音节标注。取「边数最少」的那条，与 `maximum_match` 的偏好同向。
    pub fn any_path(&self, p: usize, q: usize, max_edges: usize) -> Option<Vec<usize>> {
        if p == q {
            return Some(Vec::new());
        }
        // 反向 BFS：dist[x] = 从 x 到 q 的最少边数
        let mut dist = vec![usize::MAX; self.len + 1];
        dist[q] = 0;
        for _ in 0..max_edges {
            let mut changed = false;
            for x in (p..q).rev() {
                for &y in self.edges_from(x) {
                    if y <= q && dist[y] != usize::MAX && dist[y] + 1 < dist[x] {
                        dist[x] = dist[y] + 1;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if dist[p] == usize::MAX || dist[p] > max_edges {
            return None;
        }
        let mut out = Vec::with_capacity(dist[p]);
        let mut cur = p;
        while cur != q {
            out.push(cur - p);
            let nxt = self
                .edges_from(cur)
                .iter()
                .copied()
                .find(|&y| y <= q && dist[y] != usize::MAX && dist[y] + 1 == dist[cur])?;
            cur = nxt;
        }
        Some(out)
    }

    /// 枚举 `p→q` 之间**恰好 `n` 条边**的全部路径，各返回音节起点偏移（相对 `p`），
    /// 形态与 [`Self::any_path`] 一致。
    ///
    /// 与同族两个函数的分工——它们各自只处理一条路径（`mask_path` 验证给定的一条、
    /// `any_path` 任取最短的一条），本函数是**按约束求解**：调用方手上有一个来自图外的
    /// 约束（词条的汉字数 = 音节数），它通常能把候选路径筛到唯一；[`Self::maximum_match`]
    /// 只看 code，在等长路径间（`xi|an` / `xian`）只能猜。
    /// 见 `docs/design/pinyin-entry-boundary-contract.md` §3.1。
    ///
    /// `limit` 封顶结果数：分支爆炸只可能出现在畸形长码上（正常词条码 ≤ 12 字节、每位置
    /// 至多几条音节边）。超限即截断，调用方按「多解」处理——截断不可静默当成唯一解。
    pub fn paths_with_edges(&self, p: usize, q: usize, n: usize, limit: usize) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        if q < p || q > self.len || limit == 0 {
            return out;
        }
        if n == 0 {
            if p == q {
                out.push(Vec::new());
            }
            return out;
        }
        // 每条边至少覆盖 1 字节 ⇒ 距离放不下 n 条边时无解，省掉整棵搜索树。
        if q - p < n {
            return out;
        }
        let mut cur = Vec::with_capacity(n);
        self.walk_paths(p, q, n, p, &mut cur, &mut out, limit);
        out
    }

    /// [`Self::paths_with_edges`] 的 DFS 主体：`cur` 累积起点偏移，`pos` 为当前位置。
    #[allow(clippy::too_many_arguments)]
    fn walk_paths(
        &self,
        start: usize,
        q: usize,
        remain: usize,
        pos: usize,
        cur: &mut Vec<usize>,
        out: &mut Vec<Vec<usize>>,
        limit: usize,
    ) {
        if remain == 0 {
            if pos == q {
                out.push(cur.clone());
            }
            return;
        }
        // 剩余距离放不下剩余边数（每边至少 1 字节）→ 本分支必然走不通。
        if q.saturating_sub(pos) < remain {
            return;
        }
        for &nxt in self.edges_from(pos) {
            if nxt > q {
                break; // edges 已排序，后面只会更远
            }
            cur.push(pos - start);
            self.walk_paths(start, q, remain - 1, nxt, cur, out, limit);
            cur.pop();
            if out.len() >= limit {
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "dag_tests.rs"]
mod tests;
