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
mod tests {
    use super::*;

    fn cands(texts: &[&str]) -> Vec<Candidate> {
        texts
            .iter()
            .map(|t| Candidate {
                text: (*t).to_string(),
                ..Default::default()
            })
            .collect()
    }

    /// (显示文本, 稳定 id) 构造——模拟求值型短语候选。
    fn cands_with_id(items: &[(&str, &str)]) -> Vec<Candidate> {
        items
            .iter()
            .map(|(t, id)| Candidate {
                text: (*t).to_string(),
                id: (*id).to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn texts(c: &[Candidate]) -> Vec<String> {
        c.iter().map(|c| c.text.clone()).collect()
    }

    fn by_word(word: &str, position: usize) -> ShadowPinRule {
        ShadowPinRule::by_word(word, position)
    }

    fn by_id(word: &str, id: &str, position: usize) -> ShadowPinRule {
        ShadowPinRule {
            word: word.to_string(),
            cand_id: Some(id.to_string()),
            position,
        }
    }

    /// 删除规则记的是模板源文本（`freq_text`），候选已展开成当天文本时同样命中。
    #[test]
    fn deletes_by_template_source() {
        let mut c = cands(&["甲", "2026年11月12日"]);
        c[1].template_source = Some("$Y年$M月$D日".to_string());
        apply_shadow(&mut c, &["$Y年$M月$D日".to_string()], &[]);
        assert_eq!(texts(&c), ["甲"]);
    }

    #[test]
    fn deletes_matching_words() {
        let mut c = cands(&["甲", "乙", "丙"]);
        apply_shadow(&mut c, &["乙".to_string()], &[]);
        assert_eq!(texts(&c), ["甲", "丙"]);
    }

    #[test]
    fn pins_to_position() {
        let mut c = cands(&["甲", "乙", "丙", "丁"]);
        // 把"丙"置顶到 0。
        apply_shadow(&mut c, &[], &[by_word("丙", 0)]);
        assert_eq!(texts(&c), ["丙", "甲", "乙", "丁"]);
    }

    #[test]
    fn pins_applied_in_position_order() {
        let mut c = cands(&["甲", "乙", "丙", "丁"]);
        // 多个置顶：position 升序应用（丁→0，甲→1）。
        apply_shadow(&mut c, &[], &[by_word("甲", 1), by_word("丁", 0)]);
        // 丁 先就位到 0 → [丁,甲,乙,丙]；甲 再到 1 → [丁,甲,乙,丙]。
        assert_eq!(texts(&c), ["丁", "甲", "乙", "丙"]);
    }

    #[test]
    fn pin_position_clamped_to_end() {
        let mut c = cands(&["甲", "乙"]);
        // position 越界 → 钳到末尾。
        apply_shadow(&mut c, &[], &[by_word("甲", 99)]);
        assert_eq!(texts(&c), ["乙", "甲"]);
    }

    #[test]
    fn delete_then_pin_combined() {
        let mut c = cands(&["甲", "乙", "丙"]);
        apply_shadow(&mut c, &["甲".to_string()], &[by_word("丙", 0)]);
        assert_eq!(texts(&c), ["丙", "乙"]);
    }

    #[test]
    fn missing_pin_word_is_ignored() {
        let mut c = cands(&["甲", "乙"]);
        apply_shadow(&mut c, &[], &[by_word("不存在", 0)]);
        assert_eq!(texts(&c), ["甲", "乙"]);
    }

    #[test]
    fn same_position_latest_first() {
        // pinned 为"最新在前"：乙(index0=最近) 与 甲(index1) 都 → pos0。
        let mut c = cands(&["甲", "乙", "丙", "丁"]);
        apply_shadow(&mut c, &[], &[by_word("乙", 0), by_word("甲", 0)]);
        // 后固定的乙应在最前：[乙, 甲, 丙, 丁]
        assert_eq!(texts(&c), ["乙", "甲", "丙", "丁"]);
    }

    #[test]
    fn three_same_position_latest_first() {
        // 最新在前：丙(最近) > 乙 > 甲(最早)，都 → pos0。
        let mut c = cands(&["甲", "乙", "丙", "丁"]);
        apply_shadow(
            &mut c,
            &[],
            &[by_word("丙", 0), by_word("乙", 0), by_word("甲", 0)],
        );
        // 期望最近的排最前：[丙, 乙, 甲, 丁]
        assert_eq!(texts(&c), ["丙", "乙", "甲", "丁"]);
    }

    /// 多条规则互不挤位：A→3、B→5，B 原本排在 A 前面。逐条「摘出再插入」时 B 摘自己会把
    /// 已就位的 A 往前挤一格（A 实落第 2 位）。
    #[test]
    fn multiple_pins_do_not_shift_each_other() {
        let mut c = cands(&["x", "B", "y", "A", "z", "w", "v"]);
        let hits = apply_shadow(&mut c, &[], &[by_word("A", 3), by_word("B", 5)]);
        assert_eq!(texts(&c), ["x", "y", "z", "A", "w", "B", "v"]);
        assert_eq!(hits, 2);
    }

    /// 多条乱序（规则写入顺序与位置无关）：每条都落在自己的位置上。
    #[test]
    fn many_pins_in_any_order_land_exactly() {
        let base = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let rules = [
            by_word("h", 1),
            by_word("b", 6),
            by_word("e", 0),
            by_word("a", 4),
        ];
        let mut c = cands(&base);
        apply_shadow(&mut c, &[], &rules);
        for r in &rules {
            assert_eq!(
                c.iter().position(|x| x.text == r.word),
                Some(r.position),
                "{} 应在第 {} 位：{:?}",
                r.word,
                r.position,
                texts(&c)
            );
        }
        let mut rev = rules.clone();
        rev.reverse();
        let mut c2 = cands(&base);
        apply_shadow(&mut c2, &[], &rev);
        assert_eq!(texts(&c), texts(&c2), "位置互不相同时与规则顺序无关");
    }

    /// 同位置后写在前、先写的顺延，且不挤动其它位置上的规则。
    #[test]
    fn same_position_latest_first_keeps_other_pins() {
        // 最新在前：乙(最近) 与 甲 都 → 1；丁 → 3。
        let mut c = cands(&["丁", "甲", "乙", "丙", "戊", "己"]);
        apply_shadow(
            &mut c,
            &[],
            &[by_word("乙", 1), by_word("丁", 3), by_word("甲", 1)],
        );
        assert_eq!(texts(&c), ["丙", "乙", "甲", "丁", "戊", "己"]);
    }

    /// 越界规则钳到末尾，不影响界内规则的落点；未命中的规则不计数。
    #[test]
    fn out_of_range_pins_go_to_end_and_misses_not_counted() {
        let mut c = cands(&["甲", "乙", "丙", "丁"]);
        let hits = apply_shadow(
            &mut c,
            &[],
            &[by_word("甲", 99), by_word("丁", 1), by_word("无", 0)],
        );
        assert_eq!(texts(&c), ["乙", "丁", "丙", "甲"]);
        assert_eq!(hits, 2);
    }

    /// 删除与多条置顶同时存在：位置按删除之后的列表计。
    #[test]
    fn pins_count_positions_after_deletion() {
        let mut c = cands(&["甲", "乙", "丙", "丁", "戊"]);
        apply_shadow(
            &mut c,
            &["甲".to_string()],
            &[by_word("戊", 1), by_word("乙", 3)],
        );
        assert_eq!(texts(&c), ["丙", "戊", "丁", "乙"]);
    }

    /// `cand_id` 规则与按词规则混排，同样两阶段就位。
    #[test]
    fn id_and_word_pins_mixed() {
        let mut c = cands_with_id(&[
            ("x", ""),
            ("B", "phrase:b"),
            ("y", ""),
            ("A", ""),
            ("z", ""),
        ]);
        apply_shadow(
            &mut c,
            &[],
            &[by_word("A", 2), by_id("旧文本", "phrase:b", 3)],
        );
        assert_eq!(texts(&c), ["x", "y", "A", "B", "z"]);
    }

    /// 同一候选命中多条规则（按词一条、按 id 一条）：取 position 最小的那条，只计一次。
    #[test]
    fn one_candidate_many_rules_takes_smallest_position() {
        let mut c = cands_with_id(&[("a", ""), ("b", ""), ("X", "phrase:x"), ("d", "")]);
        let hits = apply_shadow(&mut c, &[], &[by_word("X", 3), by_id("旧", "phrase:x", 1)]);
        assert_eq!(texts(&c), ["a", "X", "b", "d"]);
        assert_eq!(hits, 1);
    }

    /// 核心回归：规则的 `word` 是**写入当天**的求值文本，今天候选文本已全变。
    /// 按 id 匹配才命中——这正是 `date` 置顶「第二天被还原」的场景。
    #[test]
    fn pin_by_id_survives_text_change() {
        let mut c = cands_with_id(&[
            ("2026年7月30日", "phrase:date:$Y年$M月$D日"),
            ("2026-07-30", "phrase:date:$Y-$MM-$DD"),
            ("2026.07.30", "phrase:date:$Y.$MM.$DD"),
        ]);
        // 昨天写下的规则：word 是昨天的文本，今天一条都对不上。
        apply_shadow(
            &mut c,
            &[],
            &[by_id("2026-07-29", "phrase:date:$Y-$MM-$DD", 0)],
        );
        assert_eq!(
            texts(&c),
            ["2026-07-30", "2026年7月30日", "2026.07.30"],
            "按 cand_id 匹配应跨日生效"
        );
    }

    /// 反向闸门：`cand_id` 非空时**不得**回退到 word 匹配。
    /// 否则过期文本会误伤当天碰巧同文的另一条候选。
    #[test]
    fn pin_by_id_does_not_fall_back_to_word() {
        // 「2026-07-29」今天仍存在，但它是**另一条**短语（固定字面量）产出的。
        let mut c = cands_with_id(&[
            ("2026-07-30", "phrase:date:$Y-$MM-$DD"),
            ("2026-07-29", "phrase:date:literal-fixed"),
        ]);
        // 规则的 id 指向 $Y-$MM-$DD，word 是过期的 2026-07-29。
        apply_shadow(
            &mut c,
            &[],
            &[by_id("2026-07-29", "phrase:date:$Y-$MM-$DD", 1)],
        );
        // 命中的必须是 id 所指的那条（移到 1 位），而不是同文的字面量那条。
        assert_eq!(texts(&c), ["2026-07-29", "2026-07-30"]);
    }

    /// 规则带 id、候选却没填 id（如码表候选）→ 不命中，不回退 word。
    /// 对齐 Go `TestApplyShadowPins_IDFallsBackWhenAbsent`。
    #[test]
    fn pin_by_id_misses_when_candidate_has_no_id() {
        let mut c = cands(&["甲", "乙", "丙"]);
        apply_shadow(&mut c, &[], &[by_id("丙", "phrase:x:stale", 0)]);
        assert_eq!(texts(&c), ["甲", "乙", "丙"], "候选无 id → 规则不生效");
    }

    /// 空字符串 cand_id 等价于「无 id」，落回 word 匹配（防 store 侧写入空串）。
    #[test]
    fn empty_cand_id_falls_back_to_word() {
        let mut c = cands(&["甲", "乙"]);
        let rule = ShadowPinRule {
            word: "乙".into(),
            cand_id: Some(String::new()),
            position: 0,
        };
        apply_shadow(&mut c, &[], &[rule]);
        assert_eq!(texts(&c), ["乙", "甲"]);
    }
}
