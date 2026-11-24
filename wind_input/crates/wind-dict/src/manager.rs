//! 词典管理器：拥有 CompositeDict，作为引擎的统一查询面。
//!
//! 见 docs/redesign/dict.md。每方案引擎各持一个 composite（EngineManager 按方案缓存引擎），
//! DictManager 是它的持有者 + 查询入口，切方案不换层。
//!
//! 词频（排序独立维度，frequency.md）与 shadow（Provider，引擎排序后应用）不在查询层。

use crate::cached::CachedDict;
use crate::composite::CompositeDict;
use crate::layer::{DictLayer, LayerType};
use wind_candidate::{Candidate, CandidateMeta, CandidateSource, better};

/// 词典管理器：持有一个方案的多层复合词典。
#[derive(Default)]
pub struct DictManager {
    composite: CompositeDict,
}

impl DictManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一层（System/User/Temp…）。
    pub fn register_layer(&self, layer: Box<dyn DictLayer>) {
        self.composite.register_layer(layer);
    }

    /// 按名注销一层，返回是否真的摘掉了。摘层即释放该层持有的词典（含 mmap 句柄）。
    pub fn unregister_layer(&self, name: &str) -> bool {
        self.composite.unregister_layer(name)
    }

    /// 运行时启停某层（按名），用于码表扩展词库热插拔。返回是否命中。
    pub fn set_layer_enabled(&self, name: &str, enabled: bool) -> bool {
        self.composite.set_layer_enabled(name, enabled)
    }

    /// 精确查找（跨层合并、排序、截断）。
    pub fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.composite.search(code, limit)
    }

    /// 前缀查找。
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.composite.search_prefix(prefix, limit)
    }

    /// 带音节上限的前缀查找，见 [`crate::layer::DictLayer::search_prefix_capped`]。
    pub fn search_prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<Candidate> {
        self.composite
            .search_prefix_capped(prefix, limit, max_syllables, completed_len)
    }

    /// 按声母串查找（简拼召回）。返回超集，判据仍在引擎侧，
    /// 见 [`crate::layer::DictLayer::search_abbrev`]。
    pub fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.composite.search_abbrev(abbrev, limit)
    }

    /// 见 [`crate::layer::DictLayer::search_abbrev_exact`]。
    pub fn search_abbrev_exact(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.composite.search_abbrev_exact(abbrev, limit)
    }

    /// 是否存在严格长于 `prefix` 的编码（跨层，见 `DictLayer::has_longer_code`）。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        self.composite.has_longer_code(prefix)
    }

    /// 通配查询（跨层，见 `DictLayer::search_pattern` / `CompositeDict::search_pattern`）。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        self.composite
            .search_pattern(pattern, wildcard, limit, with_prefix)
    }

    /// 全量枚举各启用层的 `(code, text, weight)`，供离线索引构建。
    /// O(全表)，只在索引构建这类一次性场合调用，绝不能进按键链路。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        self.composite.for_each_entry(f);
    }

    /// 见 [`CompositeDict::entries_digest`]。
    pub fn entries_digest(&self) -> Option<Vec<String>> {
        self.composite.entries_digest()
    }

    pub fn composite(&self) -> &CompositeDict {
        &self.composite
    }
}

/// 系统主词库层：把不可变 mmap `CachedDict` 包成 DictLayer（System 优先级最低）。
/// 候选 source 不在此设定（系统词库被码表/拼音引擎共用），由引擎按自身类型标注。
///
/// `enabled` 为原子标志：码表扩展词库支持运行时热插拔——禁用的扩展层仍常驻（已 mmap），
/// 仅在查询时被 composite 跳过；`set_enabled` 取 `&self`，故无需重建引擎即可即时启停。
pub struct SystemDictLayer {
    dict: CachedDict,
    /// `Arc<str>` 而非 `String`：层名要随每条候选填进 `meta.weight_layer`（供调试段标出
    /// 权重来源），而候选在按键热路径上成百上千地造——克隆一次原子计数远比堆分配便宜。
    name: std::sync::Arc<str>,
    enabled: std::sync::atomic::AtomicBool,
    /// 层级基序档位（见 DictLayer::base_order）。设计者经 [[dictionaries]].base_order 配置。
    base_order: i32,
    /// 默认权重（`[[dictionaries]].default_weight`）：Some(w) 时覆盖本库所有条目的权重为 w。
    /// 用于无权重的附加库——与带权重主库合并、按权重排序时，让其条目落在设计者选定的权重档，
    /// 而非因 weight=0 全部沉底。None = 用词库自身权重。
    default_weight: Option<i32>,
    /// 权重归一化（方案级 `[weight_spec]`，施加到本方案全部词库层）：Some 时把权重映射回
    /// 约定值域 `0~WEIGHT_RANGE_MAX`，使其与短语权重同轴可比。None = 不归一化（守约词库的常态）。
    ///
    /// 与 `default_weight` 的分工：后者抹平整库权重（退化为文件顺序），前者保序压缩。
    /// 两者同时配时 `default_weight` 优先——它是更强的声明（「本库不参与权重排序」）。
    weight_norm: Option<crate::WeightNorm>,
    /// 越界告警的一次性闸门。见 [`SystemDictLayer::warn_out_of_range`]。
    over_range_warned: std::sync::atomic::AtomicBool,
}

impl SystemDictLayer {
    /// 默认启用。
    pub fn new(dict: CachedDict, name: impl Into<String>) -> Self {
        Self::with_enabled(dict, name, true)
    }

    /// 指定初始启用状态（码表扩展层按方案配置的 enabled 传入）。
    pub fn with_enabled(dict: CachedDict, name: impl Into<String>, enabled: bool) -> Self {
        Self {
            dict,
            name: std::sync::Arc::from(name.into().as_str()),
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            base_order: 0,
            default_weight: None,
            weight_norm: None,
            over_range_warned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 链式设置层基序档位（`[[dictionaries]].base_order`）。默认 0。
    pub fn with_base_order(mut self, base_order: i32) -> Self {
        self.base_order = base_order;
        self
    }

    /// 链式设置默认权重（`[[dictionaries]].default_weight`）。Some(w) 覆盖本库所有条目权重。
    pub fn with_default_weight(mut self, default_weight: Option<i32>) -> Self {
        self.default_weight = default_weight;
        self
    }

    /// 链式设置权重归一化（`[dictionaries.weight_spec]`）。见字段文档与
    /// `docs/design/dict-weight-normalization.md`。
    pub fn with_weight_norm(mut self, weight_norm: Option<crate::WeightNorm>) -> Self {
        self.weight_norm = weight_norm;
        self
    }

    /// 本层最终对外的权重：`default_weight` 覆盖 > `weight_norm` 归一化 > 词库原值。
    ///
    /// 在查询时换算而非加载时改写：词库可能是 mmap 共享的只读产物（wdat），改不得；
    /// 且同一份词库可被多个方案以不同 `weight_spec` 引用。每次查询至多 `limit` 条、
    /// 每条两次 `ln`，开销可忽略。
    #[inline]
    fn effective_weight(&self, raw: i32) -> i32 {
        match self.default_weight {
            Some(w) => w,
            None => match &self.weight_norm {
                Some(n) => n.apply(raw),
                None => {
                    if raw > crate::WEIGHT_RANGE_MAX {
                        self.warn_out_of_range(raw);
                    }
                    raw
                }
            },
        }
    }

    /// 「权重越界且未配归一化」的一次性告警。
    ///
    /// 短语按自身权重（`0~10000`）与码表候选同场竞争；未归一的导入词库不配 `[weight_spec]`
    /// 会让短语静默沉底，本告警是唯一提示。放在查询期而非加载期：词库建了 `.wdat` 缓存后
    /// 就不再解析 yaml，解析期诊断（`ParseStats::log_weight_range`）不会再响。
    /// 见 `docs/design/dict-weight-normalization.md` §4.1。
    ///
    /// 只报一次：热路径上，且同一库的越界条目往往成千上万。
    /// 不打候选文本，只打词库名与权重数值（日志隐私红线）。
    #[cold]
    fn warn_out_of_range(&self, raw: i32) {
        use std::sync::atomic::Ordering;
        if self
            .over_range_warned
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            tracing::warn!(
                "词库 {} 出现越界权重 {}（约定值域 0~{}），且所属方案未配 [weight_spec]。\
                 短语权重在 0~{} 轴上，与之同场比较会被压到底。\
                 请跑 `wind_input dict weight-check` 体检并按建议配置方案级 [weight_spec]。",
                self.name,
                raw,
                crate::WEIGHT_RANGE_MAX,
                crate::WEIGHT_RANGE_MAX,
            );
        }
    }

    /// 系统层条目总数（日志/调试用）。
    pub fn len(&self) -> usize {
        self.dict.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dict.is_empty()
    }
}

impl DictLayer for SystemDictLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn layer_type(&self) -> LayerType {
        LayerType::System
    }

    fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    fn base_order(&self) -> i32 {
        self.base_order
    }

    /// 委托给底层 `CachedDict`，并按本层的 `default_weight` / `weight_norm` 换算权重
    /// ——与 [`Self::search`] 同域（见 trait 文档）。
    fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        self.dict.for_each_entry(&mut |code, text, weight| {
            f(code, text, self.effective_weight(weight));
        });
    }

    /// 词库文件路径 + 它的缓存摘要（[`crate::cache_fp::cache_digest`]：有 `.fp` 走内容指纹，
    /// 无则 `大小:mtime`）+ wdat 自身的 `大小:mtime` + 两项权重换算参数。
    ///
    /// 换算参数必须在内：`for_each_entry` 报的是换算后的权重，同一个 wdat 换了
    /// `default_weight` / `weight_spec`，枚举出的权重就变了，只哈希文件会复用旧权重的索引。
    /// 内存词库（wdat 写失败的降级形态）没有稳定的磁盘产物 ⇒ `None`。
    ///
    /// wdat 自身的 `大小:mtime` 挡住「文件被外部原地改写」而 `.fp` 摘要不变的情形；代价是
    /// wdat 内容没变但被重写时多重建一次词组索引（百毫秒级）。只混进本方法、不改共用的
    /// `cache_digest`：后者是 `.wridx` 的指纹源，改它会让全体用户的反查索引白重建一次。
    fn entries_digest(&self) -> Option<String> {
        let p = self.dict.source_file()?;
        let own = match std::fs::metadata(p) {
            Ok(m) => format!(
                "{}:{}",
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos())
            ),
            Err(_) => "absent".to_string(),
        };
        Some(format!(
            "sys|{}|{}|wdat={own}|dw={:?}|wn={:?}",
            p.display(),
            crate::cache_fp::cache_digest(p),
            self.default_weight,
            self.weight_norm
        ))
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        // 用 search_with_boundary 而非 search：把词典的音节真值边界带进候选（wdat v4）。
        // 非拼音词库（五笔等）与旧格式的 boundary 恒 0，消费方据此降级，行为不变。
        let mut v: Vec<Candidate> = self
            .dict
            .search_with_boundary(code)
            .into_iter()
            .map(|hit| Candidate {
                text: hit.text,
                code: code.to_string(),
                weight: self.effective_weight(hit.weight),
                // 归一化会改写 `weight`，原值留给调试段对照词库文件；`weight_layer` 记权重出自
                // 哪本词库（跨层合并后 weight 与 code 可能分属不同层）。
                meta: CandidateMeta {
                    raw_weight: hit.weight,
                    weight_layer: Some(self.name.clone()),
                    ..Default::default()
                },
                natural_order: hit.order,
                boundary: hit.boundary,
                source: CandidateSource::None,
                ..Default::default()
            })
            .collect();
        v.sort_by(better);
        if limit > 0 {
            v.truncate(limit);
        }
        v
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        let mut v: Vec<Candidate> = self
            .dict
            .search_prefix_with_boundary(prefix, limit)
            .into_iter()
            .map(|hit| Candidate {
                text: hit.text,
                code: hit.code,
                weight: self.effective_weight(hit.weight),
                // 原值与来源层留给调试段，同 `search`。
                meta: CandidateMeta {
                    raw_weight: hit.weight,
                    weight_layer: Some(self.name.clone()),
                    ..Default::default()
                },
                natural_order: hit.order,
                boundary: hit.boundary,
                source: CandidateSource::None,
                ..Default::default()
            })
            .collect();
        v.sort_by(better);
        if limit > 0 {
            v.truncate(limit);
        }
        v
    }

    /// 直接问底层有序索引（DAT 转移表 / BTreeMap），不物化任何候选。
    /// 覆盖 trait 默认实现，规避其 limit 截断导致的漏判。
    fn has_longer_code(&self, prefix: &str) -> bool {
        self.dict.has_longer_code(prefix)
    }

    /// 通配查询：委托底层有序索引，权重按本层口径换算，等长档在前。
    fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        let n = pattern.chars().count();
        let mut v: Vec<Candidate> = self
            .dict
            .search_pattern(pattern, wildcard, limit, with_prefix)
            .into_iter()
            .map(|hit| {
                let is_prefix = hit.code.chars().count() > n;
                Candidate {
                    text: hit.text,
                    code: hit.code,
                    weight: self.effective_weight(hit.weight),
                    meta: CandidateMeta {
                        raw_weight: hit.weight,
                        weight_layer: Some(self.name.clone()),
                        ..Default::default()
                    },
                    natural_order: hit.order,
                    boundary: hit.boundary,
                    is_prefix,
                    source: CandidateSource::None,
                    ..Default::default()
                }
            })
            .collect();
        v.sort_by(|a, b| crate::layer::cmp_pattern(n, a, b));
        if limit > 0 {
            v.truncate(limit);
        }
        v
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
