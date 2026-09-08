//! wind-dict: 词典系统（多层复合词典、mmap 二进制格式、DAT 格式）
//!
//! 与 Go 版本 `wind_input/internal/dict/` 对齐。

/// 词库权重的**约定值域上界**：`0 ~ WEIGHT_RANGE_MAX`。
///
/// 这条约定的意义是让**码表权重与短语权重同轴**——短语权重同样规范在 `0~10000`
/// （`data/system.phrases.toml` 头部："weight: 0~10000 …，未设置默认 1000（中位）"）。
/// 同轴才使「短语 vs 码表」的权重比较有意义，用户调短语权重才调得动先后。
///
/// ⚠️ 此前这条约定**只写在注释里**，解析端零校验。实测偏离：虎码 13% 的条目超范围
/// （max 一千万，是原始语料词频未归一化）、三个第三方方案干脆无权重列。
/// 后果是短语权重的调节能力依方案而异——五笔下健康、虎码下拉满也没用。
///
/// 现在解析期会对超范围词库出一条 `warn`（[`codetable::ParseStats`]），归一化则按库
/// opt-in（`[dictionaries.weight_spec]`）。为什么不强制归一：守约词库（如五笔 p50=941）
/// 并非均匀分布，强行拉平会无谓地改掉它的既有行为。全文见
/// `docs/design/dict-weight-normalization.md`。
pub const WEIGHT_RANGE_MAX: i32 = 10_000;

pub mod binformat;
pub mod cache_fp;
pub mod cache_ns;
pub mod cached;
pub mod codetable;
pub mod commentdict;
pub mod composite;
pub mod datformat;
pub mod emojidict;
pub mod gramdb;
pub mod hotcache;
pub mod layer;
pub mod manager;
pub mod reader_pool;
pub mod reverseidx;
pub mod store_layer;
pub mod trie;
pub mod weight_norm;

pub use composite::CompositeDict;
pub use layer::{DictLayer, LayerType, MutableLayer};
pub use manager::{DictManager, SystemDictLayer};
pub use store_layer::{StoreTempLayer, StoreUserLayer};
pub use weight_norm::WeightNorm;
