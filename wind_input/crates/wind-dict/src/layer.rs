//! 词典层接口
//!
//! 与 Go 版本 `wind_input/internal/dict/layer.go` 对齐。

use std::cmp::Ordering;
use wind_candidate::{Candidate, better};

/// 词典层类型（数值越小优先级越高）。
/// 注：Shadow（置顶/删除）**不是查询层**，而是 ShadowProvider，在引擎排序后应用
/// （见 docs/redesign/dict.md §2）——故不在此枚举中。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LayerType {
    Logic = 0,  // 命令（日期、UUID）
    User = 1,   // 用户自造词
    Temp = 2,   // 临时学习词
    Cell = 3,   // 单元词典
    System = 4, // 系统主词典
    /// 自动造词的**草稿层**：滑窗切出、尚未被任何人用过的猜测词
    /// （`docs/design/auto-phrase-draft-layer.md`）。
    ///
    /// 数值排在 `System` **之后**，与「数值越小优先级越高」一致——草稿是未经验证的猜测，
    /// 等权时该让位给一切真词。这也让 `CompositeDict::merge_search` 的跨层去重
    /// 自动做对：同 text 时保留数值更小那层的 code，草稿不会盖掉真词。
    ///
    /// ⚠️ **取 5 而不是插在 `Temp` 与 `Cell` 之间**，是为了不改动任何现有档位的数值——
    /// 那会连带改掉 `base_order` 的默认分档与 `register_layer` 的排序，
    /// 为一个新层去动四个旧层不划算。
    Draft = 5,
}

/// 词典层接口
pub trait DictLayer: Send + Sync {
    /// 层名称
    fn name(&self) -> &str;

    /// 层类型
    fn layer_type(&self) -> LayerType;

    /// 精确查找
    fn search(&self, code: &str, limit: usize) -> Vec<Candidate>;

    /// 前缀查找
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate>;

    /// **按声母串查找**（简拼召回）：返回该层里声母投影等于 `abbrev` 的词条。
    ///
    /// 返回的是**超集**，调用方仍须逐条过自己的判据（音节数、逐段全等、混合模式校验）。
    /// 本方法只负责把「候选集」从全层缩到一个声母组——判据一律留在引擎侧，
    /// 这样索引不会悄悄改变简拼的语义，只改变取候选的代价。
    ///
    /// 默认返回空：只有背后有声母索引的层才实现（当前是 `StoreUserLayer` /
    /// `StoreTempLayer`）。系统词库层走的是另一条路——`CachedDict::search_abbrev`
    /// 查 wdat 的 `AbbrevSection` 拿到**码**再回查，由引擎直接调用，不经本层接口。
    ///
    /// ⚠️ 默认实现返回空**而不是回退到全层枚举**：静默的全表扫正是简拼卡顿的根因，
    /// 与其让新层不知不觉继承那个代价，不如让它召不回、在测试里立刻暴露。
    fn search_abbrev(&self, _abbrev: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 声母串**恰为** `abbrev` 的词条：不含无边界词（`boundary == 0`）。`limit = 0` 不设上限。
    ///
    /// 与 [`Self::search_abbrev`] 的区别只在取数范围：那边为简拼召回服务，要连无边界词一起
    /// 交给引擎现切；这边给整句词图用，节点要求真值切分，无边界词必被拒收，不该占名额。
    /// 默认返回空，理由同 [`Self::search_abbrev`]。
    fn search_abbrev_exact(&self, _abbrev: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 该层是否存在**严格长于** `prefix` 的编码——「更长后继」存在性判据，供上屏安全阀
    /// （自动上屏 / 满码清空 / 顶码）使用：还能接着打就别急着替用户上屏。
    ///
    /// 默认实现沿用「取一批前缀候选再看有没有更长 code」的老办法，仅为无法廉价判断的层
    /// （redb / 内存 trie）兜底。它**受 limit 截断影响**：更长编码的候选权重偏低被挤出
    /// 前 64 名时会漏判成 false。由有序结构支撑的层应覆盖本方法直接问索引，
    /// 见 `SystemDictLayer`。
    fn has_longer_code(&self, prefix: &str) -> bool {
        let n = prefix.chars().count();
        self.search_prefix(prefix, 64)
            .iter()
            .any(|c| c.code.chars().count() > n)
    }

    /// 全量枚举本层的 `(code, text, weight)`，供**离线索引构建**使用。
    ///
    /// 消费方（都是懒建 + 后台预热的一次性全表扫，各自有失效通路）：
    /// - 码表整句的简码索引 `wind_engine::codetable::sentence`
    /// - 英文词组分词索引 `wind_engine::english_phrase`（t42）
    ///
    /// ⚠️ **不是查询接口**：它是 O(全表) 的，绝不能出现在按键链路上。
    /// 默认实现为空——与 [`Self::search_abbrev`] 同一取舍：默认「枚举不到」好过默认
    /// 「静默全表扫」，让没有廉价枚举能力的层（redb / 内存 trie）在测试里立刻暴露，
    /// 而不是不知不觉继承一份全表遍历的代价。
    ///
    /// `weight` 须是该层**对外的有效权重**（经 `default_weight` / `weight_norm` 换算后），
    /// 与 [`Self::search`] 返回的候选同域——否则索引里的权重与查询结果对不上。
    fn for_each_entry(&self, _f: &mut dyn FnMut(&str, &str, i32)) {}

    /// [`Self::for_each_entry`] 枚举结果的**稳定摘要**，供落盘的派生索引判定「盘上那份
    /// 还对不对得上本层」（当前唯一消费方：英文词组分词索引 `.wphr`）。
    ///
    /// - `Some(s)`：同一个 `s` 必须对应同一组枚举结果（**含权重换算**）；
    /// - `None`：说不清（内存词库、没有稳定磁盘产物的层）⇒ 以它为源的派生索引**不落盘**，
    ///   每次照旧全表扫——慢，但结果正确。
    ///
    /// 默认 `None`，理由同 `for_each_entry` 默认为空：覆盖了 `for_each_entry` 却忘了覆盖
    /// 本方法的层只会「没缓存」，不会把一份旧索引骗成新鲜的。反过来，**没有**覆盖
    /// `for_each_entry` 的层（枚举恒空）要参与缓存，得自己显式声明，见 `StoreUserLayer`。
    fn entries_digest(&self) -> Option<String> {
        None
    }

    /// **通配查询**（`docs/design/codetable-wildcard.md` §5.1）：`pattern` 中等于 `wildcard`
    /// 的位匹配**恰好一个**任意码元，其余位字面匹配。`with_prefix` 时追加更长编码
    /// （前 `pattern` 位匹配即可）的前缀补全。
    ///
    /// 结果**等长档优先**（[`cmp_pattern`]），各层自己先按两档取额再返回：更长编码的权重
    /// 常高于等长单字（词频 vs 字频），混排后截断会把等长结果挤出配额。
    ///
    /// 默认返回空——与 [`Self::search_abbrev`] 同一取舍：不支持的层（草稿层即刻意不支持，
    /// 见 `StoreDraftLayer` 文档）召不回，而不是静默全表扫。
    fn search_pattern(
        &self,
        _pattern: &str,
        _wildcard: char,
        _limit: usize,
        _with_prefix: bool,
    ) -> Vec<Candidate> {
        Vec::new()
    }

    /// 该层当前是否启用：禁用层在 composite 查询时被跳过（不出候选）。默认始终启用。
    /// 用于码表扩展词库的运行时热插拔——禁用的扩展层仍常驻（已 mmap），仅不参与查询。
    fn enabled(&self) -> bool {
        true
    }

    /// 运行时启停该层（支持热插拔的层覆盖此方法；默认 no-op）。
    /// 取 `&self`（内部用原子标志），故无需重建引擎即可即时生效。
    fn set_enabled(&self, _enabled: bool) {}

    /// 该层候选的**层级基序档位**：排序时 `base_order` 作为独立层级（weight 之后、
    /// natural_order 之前，见 `candidate::better`/`by_natural`），值越小越靠前。
    ///
    /// 默认按**层类型**给小整数档位：非系统层（命令/用户词/临时词/单元）恒排在系统词库层
    /// 之前（等权时）。因是独立排序层级（非加进 natural_order），**小整数即可**分档——`-1`
    /// 就能排在 `0` 前，与 natural_order 大小无关，无需魔法常量。系统层默认 0，由
    /// `SystemDictLayer` 覆盖为 `[[dictionaries]].base_order`（设计者配 0/1/2… 小整数）。
    fn base_order(&self) -> i32 {
        match self.layer_type() {
            LayerType::Logic => -4,
            LayerType::User => -3,
            LayerType::Temp => -2,
            LayerType::Cell => -1,
            LayerType::System => 0,
            // 比系统层还靠后：草稿等权时让位给一切真词（沉底的主力是
            // `Candidate::is_draft`，这里只是让两处口径一致）。
            LayerType::Draft => 1,
        }
    }
}

/// 可变词典层接口
pub trait MutableLayer: DictLayer {
    /// 添加词条
    fn add(&mut self, code: &str, text: &str, weight: i32) -> anyhow::Result<()>;

    /// 删除词条
    fn remove(&mut self, code: &str, text: &str) -> anyhow::Result<()>;

    /// 更新词条权重
    fn update(&mut self, code: &str, text: &str, new_weight: i32) -> anyhow::Result<()>;

    /// 保存到持久化存储
    fn save(&self) -> anyhow::Result<()>;
}

/// 协调器交给引擎的通配占位符。
///
/// 通配键本身（`z`、`?`）不能直接当 pattern 的通配符：首位让位后进缓冲的 `z` 是**字面**
/// 码元，与后续作通配的 `z` 同形。协调器按裁决把「作通配的那几位」替换成本字符，
/// 它不可能出现在任何码表编码里（码元来自物理按键，`\u{1}` 按不出来）。
pub const WILDCARD_SLOT: char = '\u{1}';

/// `code` 是否匹配 `pattern`：逐位比较（`wildcard` 位任意），等长即匹配；
/// `with_prefix` 时更长的 `code` 只要前 `pattern.len()` 位匹配也算。更短的 `code` 恒不匹配。
pub fn pattern_matches(pattern: &str, wildcard: char, code: &str, with_prefix: bool) -> bool {
    let mut cs = code.chars();
    for pc in pattern.chars() {
        match cs.next() {
            Some(cc) if pc == wildcard || pc == cc => {}
            _ => return false,
        }
    }
    with_prefix || cs.next().is_none()
}

/// 首个通配位之前的字面前缀（有序结构据此做 range 扫描）。首位即通配时为空串。
pub fn literal_prefix(pattern: &str, wildcard: char) -> &str {
    match pattern.find(wildcard) {
        Some(i) => &pattern[..i],
        None => pattern,
    }
}

/// 通配结果的排序：等长（`code` 字符数 == `pattern_len`）档恒在前，档内按 [`better`]。
pub fn cmp_pattern(pattern_len: usize, a: &Candidate, b: &Candidate) -> Ordering {
    let ea = a.code.chars().count() == pattern_len;
    let eb = b.code.chars().count() == pattern_len;
    eb.cmp(&ea).then_with(|| better(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matches_exact_length_and_prefix() {
        let w = '?';
        assert!(pattern_matches("a?c", w, "abc", false));
        assert!(!pattern_matches("a?c", w, "abd", false));
        assert!(
            !pattern_matches("a?c", w, "ab", false),
            "短于 pattern 不匹配：不做王码「末尾通配匹配更短码」"
        );
        assert!(
            !pattern_matches("a?c", w, "abcd", false),
            "更长码只在 with_prefix 时匹配"
        );
        assert!(pattern_matches("a?c", w, "abcd", true));
        assert!(pattern_matches("??", w, "zz", false), "首位通配");
        assert!(
            !pattern_matches("??", w, "z", true),
            "with_prefix 也不放行更短码"
        );
    }

    #[test]
    fn literal_prefix_stops_at_first_wildcard() {
        let w = '?';
        assert_eq!(literal_prefix("ab?d", w), "ab");
        assert_eq!(
            literal_prefix("?b", w),
            "",
            "首位通配 ⇒ 字面前缀为空（退化全表扫描）"
        );
        assert_eq!(literal_prefix("abc", w), "abc");
    }

    #[test]
    fn cmp_pattern_puts_equal_length_first_then_better() {
        let c = |code: &str, w: i32| Candidate {
            code: code.into(),
            weight: w,
            ..Default::default()
        };
        let mut v = [c("abcd", 9999), c("ab", 10), c("ac", 20)];
        v.sort_by(|a, b| cmp_pattern(2, a, b));
        let codes: Vec<&str> = v.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(
            codes,
            ["ac", "ab", "abcd"],
            "等长档恒在前，档内按 better（权重降序）"
        );
    }
}
