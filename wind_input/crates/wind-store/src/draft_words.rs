//! 自动造词的草稿层（运行时引擎门面；真源是 `data/<schema>/draft.dict.yaml`）
//!
//! 滑窗切出的候选词先落这里，用户真的用它上屏过一次才跃迁进临时词库
//! （[`Store::promote_draft_to_temp`]）。设计见 `docs/design/auto-phrase-draft-layer.md`。
//!
//! # 与临时词库的分界
//!
//! 草稿是机器猜的，带有效期，到期没人用就丢；临时词是用户用过的，长期留着。
//! 杂词率高是预期而非缺陷：「先记一堆、用过的才留」，过滤发生在使用端。
//!
//! # 时间语义
//!
//! 只持久化一个时间列 `last_used`：它是有效期起点、容量淘汰的排序键，同一条草稿再次写入时
//! 刷新它（见 [`Store::add_drafts`]），因此淘汰近似 LRU。
//!
//! 过期判定必须在查询时做（[`Store::search_drafts`] 带 `ttl_secs`）：
//! [`Store::purge_expired_drafts`] 只防表无限堆积，清理没跑到的窗口里过期草稿照样会被召回。

use crate::store::Store;
use crate::user_words::split_key;

/// 旧表 `DRAFT_WORDS` 的 value：定长 8 字节 —— `created_at i64`（P1 迁移读它）。
pub(crate) fn dec_draft(b: &[u8]) -> Option<i64> {
    if b.len() < 8 {
        return None;
    }
    Some(i64::from_le_bytes(b[0..8].try_into().ok()?))
}

impl Store {
    /// 批量写入草稿（一次批量写）。已存在的条目**刷新 `last_used`**（续期，见模块文档）。
    ///
    /// 调用方负责攒批：草稿的产生速率（滑窗下约每字 4 条）远高于本库其余写入方，
    /// 逐条写会频繁触发引擎落盘与文本折回。
    ///
    /// 返回实际写入条数（= `items.len()`）。
    pub fn add_drafts(&self, schema: &str, items: &[(String, String)]) -> anyhow::Result<usize> {
        let n = self.runtime().add_drafts(schema, items)?;
        self.after_write();
        Ok(n)
    }

    /// 精确查某 code 下**未过期**的草稿，返回 text 列表。
    ///
    /// 只做精确查询，**没有前缀版本**，这是设计的一部分而非省略：码表打字是前缀式的
    /// （w → wq → wqv…），草稿若参与前缀召回，滑窗那些杂词会在每一次按键上涌进候选列表。
    /// 只精确命中意味着「打满这个词的完整码才出来」，正是草稿该有的语义。
    pub fn search_drafts(
        &self,
        schema: &str,
        code: &str,
        ttl_secs: i64,
    ) -> anyhow::Result<Vec<String>> {
        Ok(self.runtime().search_drafts(schema, code, ttl_secs))
    }

    /// 草稿是否存在且未过期（跃迁前的判据）。
    pub fn has_draft(&self, schema: &str, code: &str, text: &str, ttl_secs: i64) -> bool {
        self.runtime().has_draft(schema, code, text, ttl_secs)
    }

    /// 清理过期草稿（引擎侧批量删）。返回删除条数。`ttl_secs == 0` 时什么都不做。
    ///
    /// 这是**容量维护**，不是过期语义——过期判定在 [`Self::search_drafts`] 里，
    /// 少跑一次清理只会让表大一点，不会让过期草稿被召回。
    pub fn purge_expired_drafts(&self, schema: &str, ttl_secs: i64) -> anyhow::Result<usize> {
        Ok(self
            .runtime()
            .purge_expired_drafts(Some(schema), ttl_secs)?
            .0)
    }

    /// 同上，但**不分方案扫全表**。启动时跑一次。
    ///
    /// 按方案那一版永远够不着两类表：**开关被关掉之后**（此后再没有落库，而清理只跟在
    /// 落库之后跑）、以及**用户已经不用的方案**（它的前缀再也不会被带上）。两类都是
    /// 「草稿永久躺在库里」，而它们恰恰是最该被收掉的——没人会再用到它们。
    ///
    /// 不看开关是刻意的：关掉功能的用户更需要这次清理，且因为没有新的写入，
    /// 他的草稿表会在一个 TTL 内自行排空。
    ///
    /// 返回 `(删除条数, 剩余条数)`。剩余条数是**顺带**数出来的——这次扫描本来就要
    /// 走遍全表，而它是真机重定 `draft_max_entries` 唯一的观测出口（那个默认值目前
    /// 还是纸面估算）。`ttl_secs == 0` 时不扫描，返回 `(0, 0)`。
    pub fn purge_all_expired_drafts(&self, ttl_secs: i64) -> anyhow::Result<(usize, usize)> {
        self.runtime().purge_expired_drafts(None, ttl_secs)
    }

    /// 容量淘汰：保留 `max_keep` 条，按 `last_used` **升序**淘汰（最早的先走）。返回淘汰条数。
    ///
    /// 排序键只有 `last_used`，因为草稿没有 count 可比——它一旦被用过就不再是草稿了
    /// （跃迁进临时词库，见 [`Self::promote_draft_to_temp`]）。而写入时会刷新 `last_used`
    /// （续期），所以这条升序实际上是近似 LRU，不是纯 FIFO。
    pub fn evict_drafts(&self, schema: &str, max_keep: usize) -> anyhow::Result<usize> {
        self.runtime().evict_drafts(schema, max_keep)
    }

    /// 跃迁：草稿 → 临时词库（**单写事务**）。草稿不存在时返回 `false`，什么都不做。
    ///
    /// 「用过即转正」的第一跳。此后它就是一条普通临时词：再被用到 `count++`，
    /// 累到 `promote_count` 晋升进用户词库，容量超限时按 `(count, created_at, weight)` 淘汰。
    ///
    /// 必须同时维护 `TEMP_ABBREV` 索引：主表写了、索引没写 ⇒ 那个词的简拼静默召不回。
    /// 本仓的教训是「`promote_temp_word` 写的是用户词表却住在 temp_words.rs，按文件名数必漏」
    /// （`3ff3f1fc`），这里是同一类陷阱的镜像——本函数住在 draft_words.rs，写的却是临时词表。
    ///
    /// 已存在同 `(schema, code, text)` 的临时词时沿用旧记录并 `count++`，不覆盖它的
    /// weight 与 created_at：那条已经是用户用过的词，草稿没有资格把它的历史抹掉。
    pub fn promote_draft_to_temp(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<bool> {
        let ok = self
            .runtime()
            .promote_draft_to_temp(schema, code, text, add_weight, boundary)?;
        if ok {
            self.bump_words_gen(schema);
            self.after_write();
        }
        Ok(ok)
    }

    /// 某方案当前的草稿条数（含已过期未清理的）。供容量维护与实测调参。
    pub fn count_drafts(&self, schema: &str) -> anyhow::Result<usize> {
        Ok(self.runtime().count_drafts(schema))
    }

    /// 清空某方案的全部草稿。换词库/换方案配置后调用——草稿的码是按旧词库算的，留着就是错码。
    pub fn clear_drafts(&self, schema: &str) -> anyhow::Result<usize> {
        self.runtime().clear_drafts(schema)
    }
}

/// 拆 key 供调试/导出用（与 `user_words::split_key` 同一套编码）。
pub fn parse_draft_key(key: &str) -> Option<(&str, &str, &str)> {
    split_key(key)
}

#[cfg(test)]
pub(crate) fn now_for_test() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
#[path = "draft_words_tests.rs"]
mod tests;
