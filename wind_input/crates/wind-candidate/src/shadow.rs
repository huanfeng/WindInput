//! Shadow 规则应用：对候选列表执行用户置顶/删除的重排。
//!
//! 与 Go 版本 `wind_input/internal/coordinator` 的 shadow 应用对齐。纯函数——规则数据
//! （删除词表 + 置顶项）由调用方从 store 取出后以基础类型传入，本模块不依赖 store，便于单测。

use crate::candidate::Candidate;

/// 一条置顶/移动规则（store `ShadowPin` 的无依赖镜像，使本 crate 不依赖 wind-store）。
///
/// **匹配优先级**（对齐 Go `dict.ApplyShadowPins`）：`cand_id` 非空 → **严格**按
/// [`Candidate::id`] 匹配，候选 id 为空时视为不命中，**绝不回退到 `word`**；
/// `cand_id` 为空 → 按 `word` 匹配候选文本（存量规则与静态候选的既有行为）。
///
/// ⚠ 「id 非空则不回退 word」是本结构的核心契约，不是保守写法。动态候选（`date` 等
/// 求值型短语）的 `word` 记的是**写入规则那天的求值结果**（如 `2026-07-29`），次日
/// 必然过期；一旦回退，那条过期文本会去匹配当天候选里**碰巧同文**的另一条候选，
/// 把「规则失效」升级成「规则误伤」。
#[derive(Debug, Clone)]
pub struct ShadowPinRule {
    pub word: String,
    pub cand_id: Option<String>,
    pub position: usize,
}

impl ShadowPinRule {
    /// 按 word 匹配的规则（静态候选 / 存量规则）。
    pub fn by_word(word: impl Into<String>, position: usize) -> Self {
        Self {
            word: word.into(),
            cand_id: None,
            position,
        }
    }

    /// 本规则是否命中该候选（匹配优先级见结构体文档）。
    fn matches(&self, c: &Candidate) -> bool {
        match self.cand_id.as_deref() {
            Some(id) if !id.is_empty() => !c.id.is_empty() && c.id == id,
            _ => c.text == self.word,
        }
    }
}

/// 对 `candidates` 应用 shadow 规则：
///   1. 删除 `deleted` 中出现的词（按 text 匹配）；
///   2. 把 `pinned` 各条按其匹配目标重新就位：
///      - 先摘出全部命中项，再按 `position` 升序插回（`position` 是最终列表的下标，互不挤位）；
///      - 同一 `position` 内按"最新在前"排列——`pinned` 契约为最新在前（index 0 最近），
///        排序键 `(position 升序, 原始索引降序)` 使较老项先 insert、最新项最后 insert 停在最前；
///      - 位置越界时钳到末尾；
///      - 同一候选命中多条规则时取 position 最小的那条（先处理的规则已把它摘走，后面的规则
///        找不到它），返回值只计一次。
///
/// `deleted` 是纯文本匹配，认候选的显示文本与词库原文（[`Candidate::freq_text`]）两种形态：
/// 写端记词库原文，模板系统词（`$` / `{}` 词条）在读端可能已展开成当天的文本。短语删除走
/// `set_phrase_enabled`（按 `(code, phrase_template)` 定位，本就是稳定键，见
/// `delete_candidate_by_source`），不经这里。
///
/// # 返回值：实际**就位**的置顶规则条数
///
/// 供调用方回答「用户对这个码的顺序表达过主张吗」——出简让全据此让路（见 wind-coordinator
/// 的 `short_code_yield`）。判据必须由本函数交出，而**不是**让调用方另查一次规则：
/// `(schema, code)` 的取法有 `data_schema_id` 折叠与 `shadow_code_of` 归一两道，两处各取
/// 一次迟早失配，且失配完全静默（`freq_code` 那次连红四次才把四处遗漏抓干净）。
///
/// 只数 `pinned`，**不数 `deleted`**：删除说的是「这条别出现」，与「谁排第一」不是同一
/// 维度；把删除也算进来会平白关掉大量本该发生的让位。
///
/// 数的是**命中**而非规则条数：规则指向的候选可能压根不在本次列表里（检索范围变了、
/// 被别的规则删掉了），那样的规则没产生任何位次影响，不该算作用户对本次列表的主张。
pub fn apply_shadow(
    candidates: &mut Vec<Candidate>,
    deleted: &[String],
    pinned: &[ShadowPinRule],
) -> usize {
    if !deleted.is_empty() {
        // 写端记的是词库原文（`freq_text`：模板展开前、大小写投影前）；读端此时候选可能已展开
        // （模板系统词在主路先展开再过 shadow），故两种形态都认。存量规则记的是显示文本，也认。
        candidates.retain(|c| !deleted.iter().any(|d| d == &c.text || d == c.freq_text()));
    }
    // 契约：pinned 为最新在前（store apply_pin insert(0) + coordinator 原序传入）。
    // 排序键 (position 升序, 原始索引降序)：同 position 内让较老项先 insert，
    // 最新项最后 insert 停在最前，得到"后添加者靠前"。
    let mut idx: Vec<(usize, &ShadowPinRule)> = pinned.iter().enumerate().collect();
    idx.sort_by(|a, b| a.1.position.cmp(&b.1.position).then(b.0.cmp(&a.0)));
    // 两阶段：先把全部命中项摘出，再按上面的次序插回。逐条「摘出再插入」时，后处理的规则
    // 摘自己若原位置在先就位者之前，会把先就位的往前挤一格（A→3、B→5 而 B 原本在 A 前，
    // A 实落第 2 位）。
    let mut taken: Vec<(&ShadowPinRule, Candidate)> = Vec::new();
    for (_, rule) in idx {
        if let Some(cur) = candidates.iter().position(|c| rule.matches(c)) {
            taken.push((rule, candidates.remove(cur)));
        }
    }
    let hits = taken.len();
    for (rule, cand) in taken {
        let at = rule.position.min(candidates.len());
        candidates.insert(at, cand);
    }
    hits
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod tests;
