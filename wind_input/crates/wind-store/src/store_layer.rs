//! Store 桥接层：把 wind-store 的用户词 / 临时词包成 `DictLayer`，挂进 CompositeDict。
//!
//! 见 docs/redesign/dict.md §3。词频/shadow 不在此（词频是排序独立维度 frequency.md；
//! shadow 是 ShadowProvider，引擎排序后应用）——本文件只负责"用户词/临时词作为查询层"。

use crate::Store;
use std::sync::Arc;
use wind_candidate::Candidate;
use wind_dict::{DictLayer, LayerType};

/// 用户造词层（P2 起数据在 `user/<schema>/user.dict.yaml` + 运行时引擎；本类型是
/// `UserDataUserLayer` 的薄包装，保留 `StoreUserLayer::new(store, schema)` 这个入口，调用方不动）。
/// 构造时把该方案绑定到文本（之后手改文件经 sync 生效）。
pub struct StoreUserLayer {
    inner: wind_userdata::layer::UserDataUserLayer,
    /// 查询结果的权重下限（`None` = 原样）。见 [`Self::with_min_weight`]。
    min_weight: Option<i32>,
}

impl StoreUserLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>) -> Self {
        let schema_id = schema_id.into();
        store.ensure_user_text(&schema_id);
        Self {
            inner: wind_userdata::layer::UserDataUserLayer::new(
                store.runtime().data().clone(),
                &schema_id,
            ),
            min_weight: None,
        }
    }

    /// 查询出的候选权重至少取 `w`（只改查询结果，存储里的权重不动）。
    ///
    /// 英文用：系统词按词频名次赋了权（`wind_dict::freq_rank`，至多 `RANK_WEIGHT_MAX`），
    /// 而用户词可能是 0（导入的词表没有权重列）或手填的小值，不托底就会沉到常用系统词之后
    /// ——名次引入前系统词全是 0，用户词恒在前。代价：低于下限的几条用户词之间不再按各自
    /// 权重分先后（同权后按自然序），高于下限的照旧。
    pub fn with_min_weight(mut self, w: i32) -> Self {
        self.min_weight = Some(w);
        self
    }

    fn floor(&self, mut v: Vec<Candidate>) -> Vec<Candidate> {
        if let Some(min) = self.min_weight {
            for c in &mut v {
                c.weight = c.weight.max(min);
            }
        }
        v
    }
}

/// 全部转发给引擎层：名字（`user:{schema}`）、稳定摘要、精确 / 前缀 / 简拼（分组索引，无边界词走
/// 首字符兜底组）/ 通配，与 redb 版同语义；`natural_order` 取文本行号（未折回的新词排最后）。
impl DictLayer for StoreUserLayer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn layer_type(&self) -> LayerType {
        self.inner.layer_type()
    }

    fn entries_digest(&self) -> Option<String> {
        self.inner.entries_digest()
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.floor(self.inner.search(code, limit))
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.floor(self.inner.search_prefix(prefix, limit))
    }

    fn search_prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<Candidate> {
        self.floor(
            self.inner
                .search_prefix_capped(prefix, limit, max_syllables, completed_len),
        )
    }

    fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.floor(self.inner.search_abbrev(abbrev, limit))
    }

    fn search_abbrev_exact(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.floor(self.inner.search_abbrev_exact(abbrev, limit))
    }

    fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        self.floor(
            self.inner
                .search_pattern(pattern, wildcard, limit, with_prefix),
        )
    }
}

/// 临时学习词层（P1 起数据在运行时引擎；本类型是 `UserDataTempLayer` 的薄包装，保留
/// `StoreTempLayer::new(store, schema)` 这个入口，调用方不动）。
pub struct StoreTempLayer {
    inner: wind_userdata::layer::UserDataTempLayer,
}

impl StoreTempLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>) -> Self {
        let schema_id = schema_id.into();
        Self {
            inner: wind_userdata::layer::UserDataTempLayer::new(
                store.runtime().data().clone(),
                &schema_id,
            ),
        }
    }
}

/// 全部转发给引擎层：名字（`temp:{schema}`）、稳定摘要（不覆盖 `for_each_entry`，对派生索引
/// 的贡献恒为「无」）、精确 / 前缀（`is_prefix` 恒 true）/ 简拼（走分组索引，不回退全表扫）/
/// 通配（字面前缀扫描后过滤，不带 limit 进扫描）都与 redb 版同语义。
impl DictLayer for StoreTempLayer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn layer_type(&self) -> LayerType {
        self.inner.layer_type()
    }

    fn entries_digest(&self) -> Option<String> {
        self.inner.entries_digest()
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search(code, limit)
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search_prefix(prefix, limit)
    }

    fn search_prefix_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<Candidate> {
        self.inner
            .search_prefix_capped(prefix, limit, max_syllables, completed_len)
    }

    fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search_abbrev(abbrev, limit)
    }

    fn search_abbrev_exact(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search_abbrev_exact(abbrev, limit)
    }

    fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        self.inner
            .search_pattern(pattern, wildcard, limit, with_prefix)
    }
}

/// 自动造词的**草稿层**（P1 起数据在运行时引擎；本类型是 `UserDataDraftLayer` 的薄包装）。
///
/// 滑窗切出的猜测词住在这里，用户真的用它上屏过一次才跃迁进临时词库
/// （设计见 `docs/design/auto-phrase-draft-layer.md`）。
///
/// # 只响应精确查询 —— 这是设计的一部分，不是没实现完
///
/// [`search_prefix`](DictLayer::search_prefix) **恒返回空**。草稿层的杂词率极高
/// （模型使然：先记一堆、用过的才留），而码表打字是前缀式的（w → wq → wqv…）——
/// 草稿若参与前缀召回，那些杂词会在**每一次按键**上涌进候选列表。
/// 只精确命中意味着「打满这个词的完整码才出来」，正是草稿该有的语义，
/// 也把杂词整个挡在了前缀阶段之外。
///
/// [`search_abbrev`](DictLayer::search_abbrev) 走 trait 的默认实现（返回空、不回退全表扫）：
/// 草稿表**刻意没有简拼索引**，那是为了不让写放大跟着草稿的写入量翻上去。
/// 简拼召回等草稿跃迁进临时词库之后自然就有。
///
/// 通配查询（`search_pattern`）同样走默认空实现，理由相同。
pub struct StoreDraftLayer {
    inner: wind_userdata::layer::UserDataDraftLayer,
}

impl StoreDraftLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>, ttl_secs: i64) -> Self {
        let schema_id = schema_id.into();
        Self {
            inner: wind_userdata::layer::UserDataDraftLayer::new(
                store.runtime().data().clone(),
                &schema_id,
                ttl_secs,
            ),
        }
    }
}

impl DictLayer for StoreDraftLayer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn layer_type(&self) -> LayerType {
        self.inner.layer_type()
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search(code, limit)
    }

    /// 恒空，理由见结构体文档——**不要顺手实现它**。
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.inner.search_prefix(prefix, limit)
    }
}

#[cfg(test)]
#[path = "store_layer_tests.rs"]
mod tests;
