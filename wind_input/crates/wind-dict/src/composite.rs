//! 多层复合词典
//!
//! 与 Go 版本 `wind_input/internal/dict/composite.go` 对齐。

use crate::layer::DictLayer;
use std::collections::HashMap;
use std::sync::RwLock;
use wind_candidate::Candidate;

/// 多层复合词典
#[derive(Default)]
pub struct CompositeDict {
    layers: RwLock<Vec<Box<dyn DictLayer>>>,
}

/// `merge_search` 的查询种类：决定问各层哪个方法，其余合并逻辑三者共用。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Query {
    /// 精确码
    Exact,
    /// 前缀补全
    Prefix,
    /// 带音节上限的前缀补全（见 `DictLayer::search_prefix_capped`）
    PrefixCapped {
        max_syllables: u32,
        completed_len: usize,
    },
    /// 声母串（简拼召回）
    Abbrev,
    /// 声母串恰为本组、不含无边界词（整句词图用，见 `DictLayer::search_abbrev_exact`）
    AbbrevExact,
    /// 通配（见 `DictLayer::search_pattern`）：按 `(text, code)` 去重、等长档优先。
    Pattern { wildcard: char, with_prefix: bool },
}

impl CompositeDict {
    pub fn new() -> Self {
        Self {
            layers: RwLock::new(Vec::new()),
        }
    }

    /// 注册词典层（按 layer_type 稳定排序：相同类型保持注册顺序，
    /// 故同为 System 的主库与扩展库按注册先后决定层内优先级）。
    pub fn register_layer(&self, layer: Box<dyn DictLayer>) {
        let mut layers = self.layers.write().unwrap();
        layers.push(layer);
        layers.sort_by_key(|l| l.layer_type() as u8);
    }

    /// 按名注销词典层，返回是否真的摘掉了。
    ///
    /// 摘层会 drop 掉那个 `Box<dyn DictLayer>`，连带释放它持有的 `CachedDict`
    /// —— 对 mmap 词库这是释放文件句柄的唯一途径（Windows 下文件被 mmap 期间
    /// 删不掉，见 `reader_pool` 的模块注释）。返回值用于区分「摘掉了」与「本就没有」。
    pub fn unregister_layer(&self, name: &str) -> bool {
        let mut layers = self.layers.write().unwrap();
        let before = layers.len();
        layers.retain(|l| l.name() != name);
        layers.len() != before
    }

    /// 运行时启停某层（按名）：用于码表扩展词库热插拔，无需重建引擎。
    /// 返回是否命中该层。仅需读锁（层的 enabled 是内部原子标志）。
    pub fn set_layer_enabled(&self, name: &str, enabled: bool) -> bool {
        let layers = self.layers.read().unwrap();
        let mut hit = false;
        for l in layers.iter() {
            if l.name() == name {
                l.set_enabled(enabled);
                hit = true;
            }
        }
        hit
    }

    /// 精确查找：跨层合并去重。
    pub fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(code, limit, Query::Exact)
    }

    /// 前缀查找：跨层合并去重。
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(prefix, limit, Query::Prefix)
    }

    /// 带音节上限的前缀查找，见 [`DictLayer::search_prefix_capped`]。
    pub fn search_prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<Candidate> {
        self.merge_search(
            prefix,
            limit,
            Query::PrefixCapped {
                max_syllables,
                completed_len,
            },
        )
    }

    /// 按声母串查找（简拼召回）：跨层合并去重，见 [`DictLayer::search_abbrev`]。
    ///
    /// 走同一套 `merge_search` 而不是各层结果直接拼接，以保住跨层合并语义（同码同文权重取 max）；
    /// 拼接后由引擎先到先得去重会让排序静默变化。
    pub fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(abbrev, limit, Query::Abbrev)
    }

    /// 见 [`crate::layer::DictLayer::search_abbrev_exact`]。
    pub fn search_abbrev_exact(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(abbrev, limit, Query::AbbrevExact)
    }

    /// 通配查询：跨层合并。与其它查询的两处不同——去重键是 `(text, code)`（同字不同码各留
    /// 一条，学码要看到每个码位；同码才合并取更高权重），排序等长档优先（`cmp_pattern`）。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        self.merge_search(
            pattern,
            limit,
            Query::Pattern {
                wildcard,
                with_prefix,
            },
        )
    }

    /// 是否存在**严格长于** `prefix` 的编码：任一**启用**层命中即 true，命中即短路。
    ///
    /// 刻意不经 `merge_search`——那条路会按 text 去重并「同 text 取最短码」
    /// （见 `merge_search` 注释），把一个字的长码换成它在别层的短码，正好抹掉本判据
    /// 要找的信息。存在性判断必须逐层原样问。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        let layers = self.layers.read().unwrap();
        layers
            .iter()
            .filter(|l| l.enabled())
            .any(|l| l.has_longer_code(prefix))
    }

    /// 全量枚举各启用层的 `(code, text, weight)`，供离线索引构建（见 `DictLayer::for_each_entry`）。
    /// 不去重、不排序：跨层同 `(code, text)` 各报一次，由调用方按自己的语义合并。
    /// O(全表)，绝不能出现在按键链路上。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        let layers = self.layers.read().unwrap();
        for l in layers.iter().filter(|l| l.enabled()) {
            l.for_each_entry(f);
        }
    }

    /// [`Self::for_each_entry`] 全部输入的摘要：各启用层的 [`DictLayer::entries_digest`]，按层序。
    /// 任一层说不清即 `None`（派生索引不落盘）。过滤条件与层序必须与 `for_each_entry` 逐字一致。
    pub fn entries_digest(&self) -> Option<Vec<String>> {
        let layers = self.layers.read().unwrap();
        layers
            .iter()
            .filter(|l| l.enabled())
            .map(|l| l.entries_digest())
            .collect()
    }

    /// 跨层合并：遍历各层收集候选，按 text 去重——
    ///   - 保留高优先级层(先出现)的词条信息(code/natural_order)；
    ///   - 同码时继承后续层的更高权重；
    ///   - 前缀查询时，同 text 多码取最短码(离输入最近)及其更小 natural_order；
    ///   - 每层写入该层 `base_order()`（[[dictionaries]].base_order），
    ///     使等权/`base_sort=natural` 时按设计者指定的层间基序排列。
    fn merge_search(&self, query: &str, limit: usize, kind: Query) -> Vec<Candidate> {
        let layers = self.layers.read().unwrap();
        let mut results: Vec<Candidate> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        // 「同 text 取最短码」只对前缀查询成立（各层给的是不同长度的补全码）。精确与简拼给的
        // 都是整词码；简拼尤其不能换，候选 code 必须留全拼码供词频记账。
        let is_prefix = matches!(kind, Query::Prefix | Query::PrefixCapped { .. });
        let by_code = matches!(kind, Query::Pattern { .. });

        for layer in layers.iter() {
            // 禁用层（如关闭的码表扩展词库）跳过。
            if !layer.enabled() {
                continue;
            }
            let layer_results = match kind {
                Query::Exact => layer.search(query, limit),
                Query::Prefix => layer.search_prefix(query, limit),
                Query::PrefixCapped {
                    max_syllables,
                    completed_len,
                } => layer.search_prefix_capped(query, limit, max_syllables, completed_len),
                Query::Abbrev => layer.search_abbrev(query, limit),
                Query::AbbrevExact => layer.search_abbrev_exact(query, limit),
                Query::Pattern {
                    wildcard,
                    with_prefix,
                } => layer.search_pattern(query, wildcard, limit, with_prefix),
            };
            // 层级基序档位：写入候选的 base_order 字段（独立排序层级，不折进 natural_order）。
            let layer_base_order = layer.base_order();
            for mut cand in layer_results {
                cand.base_order = layer_base_order;
                let key = if by_code {
                    format!("{}\u{0}{}", cand.text, cand.code)
                } else {
                    cand.text.clone()
                };
                if let Some(&idx) = seen.get(&key) {
                    // 同 text 已存在：编码也相同时才继承更高权重。权重属于 `(code, text)` 词条：
                    // 不同码位（简码 `a` 与全码 `ab`）是两个独立词条，跨码位取 max 会让候选带着
                    // 别的码位的权重排序，码表方案尤其不行。
                    // code/natural_order/boundary 保留首个出现层的值；boundary 随 code 走，
                    // 不可从系统层「借」边界给用户层的码。
                    let existing = &mut results[idx];
                    if cand.code == existing.code && cand.weight > existing.weight {
                        existing.weight = cand.weight;
                        // 权重来源必须随权重一起换，否则调试段会把权重记在错的库头上。
                        existing.meta.weight_layer = cand.meta.weight_layer.take();
                    }
                    // 被丢弃这条的来源归属并入幸存者：右键删除按这两个标记决定去 `user_words`
                    // 还是 `temp_words` 删，而 redb 删不存在的 key 静默成功，丢了标记就删不掉。
                    existing.meta.is_user_dict |= cand.meta.is_user_dict;
                    existing.meta.is_temp_dict |= cand.meta.is_temp_dict;
                    // 被丢弃这条所占的码位并入幸存者：跨层同 text 常有不同码（用户层手输码 vs
                    // 系统层全码），丢掉即让「检索范围」过滤看不见该码位的常用性，见
                    // `Candidate::merged_codes`。删除侧也读它——两层码不同时，幸存者的 `code`
                    // 只对得上其中一张表。
                    existing.absorb_codes_from(&cand);
                    // 前缀：保留最短码及其更早出现位置。
                    // boundary 是 code 的音节切分，必须与 code 同进同出。
                    if is_prefix && cand.code.len() < existing.code.len() {
                        let old_code = std::mem::replace(&mut existing.code, cand.code.clone());
                        // 换码即改换词条，weight 与其来源层须与 code 整组同换（权重恒随自己的码位走）。
                        existing.weight = cand.weight;
                        existing.meta.weight_layer = cand.meta.weight_layer.take();
                        existing.boundary = cand.boundary;
                        if cand.natural_order < existing.natural_order {
                            existing.natural_order = cand.natural_order;
                        }
                        // 让位给短码的旧码位转入 merged_codes；新主码则从中剔除（主码不重复记）。
                        existing.absorb_code(&old_code);
                        existing.merged_codes.retain(|c| c != &cand.code);
                    }
                    continue;
                }
                seen.insert(key, results.len());
                results.push(cand);
            }
        }

        if by_code {
            let n = query.chars().count();
            results.sort_by(|a, b| crate::layer::cmp_pattern(n, a, b));
        } else {
            results.sort_by(wind_candidate::better);
        }
        // limit==0 视为「无上限」（与各 DictLayer::search 的 `if limit>0` 守卫、Go
        // searchInternal 一致），仅在 limit>0 时截断。调用方需要空结果时不应传 0。
        if limit > 0 && results.len() > limit {
            results.truncate(limit);
        }
        results
    }
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;
