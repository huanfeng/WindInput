//! 词典层接口

use std::cmp::Ordering;
use wind_candidate::{Candidate, better};

/// 词典层类型（数值越小优先级越高）。
/// 注：Shadow（置顶/删除）不是查询层，而是 ShadowProvider，在引擎排序后应用
/// （见 docs/redesign/dict.md §2）——故不在此枚举中。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LayerType {
    Logic = 0,  // 命令（日期、UUID）
    User = 1,   // 用户自造词
    Temp = 2,   // 临时学习词
    Cell = 3,   // 单元词典
    System = 4, // 系统主词典
    /// 自动造词的草稿层：滑窗切出、尚未被用过的猜测词（`docs/design/auto-phrase-draft-layer.md`）。
    ///
    /// 排在 `System` 之后：等权时让位给一切真词，`CompositeDict::merge_search` 去重时也不会
    /// 盖掉真词的 code。取 5 而不插在中间，是为了不改动现有档位的数值（`base_order` 默认分档
    /// 与 `register_layer` 排序都依赖它们）。
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

    /// 带音节上限的前缀查找：只取 [`crate::cached::prefix_capped_keep`] 放行的条目，
    /// `limit` 名额只发给合格条目（拼音短输入严格档，见 `search_prefix_syllable_capped`）。
    ///
    /// 默认实现先按 [`Self::search_prefix`] 取再过滤，名额可能被不合格条目占掉；背后是
    /// wdat 的层应覆盖它，让过滤参与剪枝。
    fn search_prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<Candidate> {
        let mut v = self.search_prefix(prefix, limit);
        v.retain(|c| {
            crate::cached::prefix_capped_keep(
                c.boundary,
                c.code.len(),
                max_syllables,
                completed_len,
            )
        });
        v
    }

    /// 按声母串查找（简拼召回）：返回该层里声母投影等于 `abbrev` 的词条。
    ///
    /// 返回的是超集，判据（音节数、逐段全等、混合模式校验）一律留在引擎侧逐条过。
    /// 只有背后有声母索引的层才实现（`StoreUserLayer` / `StoreTempLayer`）；系统词库层由引擎
    /// 直接调 `CachedDict::search_abbrev`，不经本接口。
    ///
    /// 默认返回空而不是回退到全层枚举：静默的全表扫会造成简拼卡顿，宁可召不回、在测试里暴露。
    fn search_abbrev(&self, _abbrev: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 声母串恰为 `abbrev` 的词条：不含无边界词（`boundary == 0`）。`limit = 0` 不设上限。
    ///
    /// 与 [`Self::search_abbrev`] 的区别只在取数范围：那边为简拼召回服务，要连无边界词一起
    /// 交给引擎现切；这边给整句词图用，节点要求真值切分，无边界词必被拒收，不该占名额。
    /// 默认返回空，理由同 [`Self::search_abbrev`]。
    fn search_abbrev_exact(&self, _abbrev: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 该层是否存在严格长于 `prefix` 的编码，供上屏安全阀（自动上屏 / 满码清空 / 顶码）使用：
    /// 还能接着打就别急着替用户上屏。
    ///
    /// 默认实现取前 64 条前缀候选来看，受截断影响会漏判成 false，仅为 redb / 内存 trie 兜底；
    /// 由有序结构支撑的层应覆盖本方法直接问索引，见 `SystemDictLayer`。
    fn has_longer_code(&self, prefix: &str) -> bool {
        let n = prefix.chars().count();
        self.search_prefix(prefix, 64)
            .iter()
            .any(|c| c.code.chars().count() > n)
    }

    /// 全量枚举本层的 `(code, text, weight)`，供离线索引构建使用（码表整句简码索引
    /// `wind_engine::codetable::sentence`、英文词组分词索引 `wind_engine::english_phrase`）。
    ///
    /// 不是查询接口：O(全表)，绝不能出现在按键链路上。默认为空，取舍同 [`Self::search_abbrev`]。
    /// `weight` 须是该层对外的有效权重（经 `default_weight` / `weight_norm` 换算后），
    /// 与 [`Self::search`] 返回的候选同域。
    fn for_each_entry(&self, _f: &mut dyn FnMut(&str, &str, i32)) {}

    /// [`Self::for_each_entry`] 枚举结果的稳定摘要，供落盘的派生索引（如英文词组分词索引
    /// `.wphr`）判定盘上那份是否仍对得上本层。
    ///
    /// - `Some(s)`：同一个 `s` 必须对应同一组枚举结果（含权重换算）；
    /// - `None`：说不清（内存词库等）⇒ 以它为源的派生索引不落盘，每次全表扫。
    ///
    /// 默认 `None`：忘了覆盖的层只会「没缓存」，不会把旧索引骗成新鲜的。枚举恒空的层要参与
    /// 缓存须显式声明，见 `StoreUserLayer`。
    fn entries_digest(&self) -> Option<String> {
        None
    }

    /// 通配查询（`docs/design/codetable-wildcard.md` §5.1）：`pattern` 中等于 `wildcard`
    /// 的位匹配恰好一个任意码元，其余位字面匹配。`with_prefix` 时追加更长编码
    /// （前 `pattern` 位匹配即可）的前缀补全。
    ///
    /// 结果等长档优先（[`cmp_pattern`]），各层自己先按两档取额再返回：更长编码的权重
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

    /// 该层候选的层级基序档位：排序时 `base_order` 作为独立层级（weight 之后、
    /// natural_order 之前，见 `candidate::better`/`by_natural`），值越小越靠前。
    ///
    /// 默认按层类型给负整数档位，使非系统层在等权时恒排在系统词库层之前。系统层默认 0，
    /// 由 `SystemDictLayer` 覆盖为 `[[dictionaries]].base_order`。
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
/// 通配键本身（`z`、`?`）不能直接当 pattern 的通配符：首位让位后进缓冲的 `z` 是字面
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
#[path = "layer_tests.rs"]
mod tests;
