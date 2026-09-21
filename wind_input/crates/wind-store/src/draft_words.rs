//! 自动造词的草稿层（redb `DRAFT_WORDS` 表）
//!
//! 滑窗切出的候选词先落这里，用户真的用它上屏过一次才跃迁进临时词库
//! （[`Store::promote_draft_to_temp`]）。设计见 `docs/design/auto-phrase-draft-layer.md`。
//!
//! # 与临时词库的分界
//!
//! **谁验证过它**：草稿是机器猜的，带有效期，到期没人用就丢；临时词是用户用过的，长期留着。
//! 杂词率高是本层的**预期而非缺陷**——整个模型的重心就是「先记一堆、用过的才留」，
//! 过滤发生在使用端而不是产生端。
//!
//! # 时间语义
//!
//! `created_at` 既是有效期的起点，也是容量淘汰的排序键，还承担「续期」：
//! 同一条草稿被再次写入时**刷新**它（见 [`Store::add_drafts`]）。于是容量淘汰
//! 「按 created_at 升序」实际上是近似 LRU——反复打的词自然排到后面。
//!
//! ⚠️ 本层的过期判定**必须在查询时做**（[`Store::search_drafts`] 带 `ttl_secs`），
//! 不能只靠 [`Store::purge_expired_drafts`] 定期清理：清理线程没跑到的窗口里，
//! 过期草稿照样会被召回。清理只是为了不让表无限堆积，不是过期语义的实现。

use crate::store::{DRAFT_WORDS, Store, TEMP_ABBREV, TEMP_WORDS};
use crate::user_words::{dec_val, enc_key, enc_val, now_secs, split_key};
use redb::ReadableTable;

/// value: 定长 8 字节 —— `created_at i64`。
///
/// 长度守卫刻意宽松（`< 8`），与 `user_words::dec_val` 同一套惰性升级思路：将来若要加字段，
/// 旧记录仍能解出 created_at，不需要 migration。
fn enc_draft(created_at: i64) -> [u8; 8] {
    created_at.to_le_bytes()
}

fn dec_draft(b: &[u8]) -> Option<i64> {
    if b.len() < 8 {
        return None;
    }
    Some(i64::from_le_bytes(b[0..8].try_into().ok()?))
}

/// 该草稿是否已过期。`ttl_secs == 0` = 永不过期。
fn is_expired(created_at: i64, now: i64, ttl_secs: i64) -> bool {
    ttl_secs > 0 && now.saturating_sub(created_at) >= ttl_secs
}

impl Store {
    /// 批量写入草稿（**单写事务**）。已存在的条目**刷新 `created_at`**（续期，见模块文档）。
    ///
    /// 批量是刚性要求而非优化：redb 是单写者，草稿的产生速率（滑窗下约每字 4 条）
    /// 远高于这个库里的其它写入方，逐条开事务会顶到选词写词频（`record_freq`）那条路上。
    /// 调用方负责攒批，本函数只保证「这一批共用一个事务」。
    ///
    /// 返回实际写入条数（= `items.len()`，失败整批回滚）。
    pub fn add_drafts(&self, schema: &str, items: &[(String, String)]) -> anyhow::Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let now = now_secs();
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                for (code, text) in items {
                    let key = enc_key(schema, code, text);
                    t.insert(key.as_str(), enc_draft(now).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(items.len())
        })
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
        let prefix = format!("{schema}\u{0}{code}\u{0}");
        let now = now_secs();
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(DRAFT_WORDS)?;
            let mut out = Vec::new();
            for item in t.range(prefix.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&prefix) {
                    break;
                }
                let Some(ca) = dec_draft(v.value()) else {
                    continue;
                };
                if is_expired(ca, now, ttl_secs) {
                    continue;
                }
                out.push(key[prefix.len()..].to_string());
            }
            Ok(out)
        })
    }

    /// 草稿是否存在且未过期（跃迁前的判据）。
    pub fn has_draft(&self, schema: &str, code: &str, text: &str, ttl_secs: i64) -> bool {
        let key = enc_key(schema, code, text);
        let now = now_secs();
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(DRAFT_WORDS)?;
            Ok(t.get(key.as_str())?
                .and_then(|g| dec_draft(g.value()))
                .is_some_and(|ca| !is_expired(ca, now, ttl_secs)))
        })
        .unwrap_or(false)
    }

    /// 清理过期草稿（单写事务）。返回删除条数。`ttl_secs == 0` 时什么都不做。
    ///
    /// 这是**容量维护**，不是过期语义——过期判定在 [`Self::search_drafts`] 里，
    /// 少跑一次清理只会让表大一点，不会让过期草稿被召回。
    pub fn purge_expired_drafts(&self, schema: &str, ttl_secs: i64) -> anyhow::Result<usize> {
        if ttl_secs <= 0 {
            return Ok(0);
        }
        let scan = format!("{schema}\u{0}");
        let now = now_secs();
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let mut deleted = 0usize;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                // 先在事务内收集再删除（无 TOCTOU，同 evict_temp_words 的形态）。
                let mut doomed: Vec<String> = Vec::new();
                for item in t.range(scan.as_str()..)? {
                    let (k, v) = item?;
                    let key = k.value();
                    if !key.starts_with(&scan) {
                        break;
                    }
                    match dec_draft(v.value()) {
                        // 解不出 created_at 的记录一并清掉：它没有可判定的有效期，
                        // 留着只会在每次查询里被 `continue` 跳过，永远占着位置。
                        None => doomed.push(key.to_string()),
                        Some(ca) if is_expired(ca, now, ttl_secs) => doomed.push(key.to_string()),
                        Some(_) => {}
                    }
                }
                for key in &doomed {
                    t.remove(key.as_str())?;
                    deleted += 1;
                }
            }
            txn.commit()?;
            Ok(deleted)
        })
    }

    /// 容量淘汰：保留 `max_keep` 条，按 `created_at` **升序**淘汰（最早的先走）。返回淘汰条数。
    ///
    /// 排序键只有 `created_at`，因为草稿没有 count 可比——它一旦被用过就不再是草稿了
    /// （跃迁进临时词库，见 [`Self::promote_draft_to_temp`]）。而写入时会刷新 `created_at`
    /// （续期），所以这条升序实际上是近似 LRU，不是纯 FIFO。
    pub fn evict_drafts(&self, schema: &str, max_keep: usize) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let mut deleted = 0usize;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                let mut all: Vec<(String, i64)> = Vec::new();
                for item in t.range(scan.as_str()..)? {
                    let (k, v) = item?;
                    let key = k.value();
                    if !key.starts_with(&scan) {
                        break;
                    }
                    // 解不出 created_at 的排在最前（i64::MIN）＝最先被淘汰，同 purge 的判据。
                    all.push((key.to_string(), dec_draft(v.value()).unwrap_or(i64::MIN)));
                }
                if all.len() > max_keep {
                    all.sort_by_key(|(_, ca)| *ca);
                    for (key, _) in all.iter().take(all.len() - max_keep) {
                        t.remove(key.as_str())?;
                        deleted += 1;
                    }
                }
            }
            txn.commit()?;
            Ok(deleted)
        })
    }

    /// 跃迁：草稿 → 临时词库（**单写事务**）。草稿不存在时返回 `false`，什么都不做。
    ///
    /// 「用过即转正」的第一跳。此后它就是一条普通临时词：再被用到 `count++`，
    /// 累到 `promote_count` 晋升进用户词库，容量超限时按 `(count, created_at, weight)` 淘汰。
    ///
    /// ⚠️ **必须同时维护 `TEMP_ABBREV` 索引**：主表写了、索引没写 ⇒ 那个词的简拼静默召不回。
    /// 本仓的教训是「`promote_temp_word` 写的是用户词表却住在 temp_words.rs，按文件名数必漏」
    /// （`3ff3f1fc`），这里是同一类陷阱的镜像——本函数住在 draft_words.rs，写的却是临时词表。
    ///
    /// 已存在同 `(schema, code, text)` 的临时词时**沿用旧记录并 `count++`**，不覆盖它的
    /// weight 与 created_at：那条已经是用户用过的词，草稿没有资格把它的历史抹掉。
    pub fn promote_draft_to_temp(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<bool> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let promoted;
            {
                let mut d = txn.open_table(DRAFT_WORDS)?;
                if d.get(key.as_str())?.is_none() {
                    promoted = false;
                } else {
                    d.remove(key.as_str())?;
                    let mut t = txn.open_table(TEMP_WORDS)?;
                    let existing = t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                    let (w, c, ca, b) = match existing {
                        Some((ow, oc, oca, ob)) => {
                            (ow, oc + 1, oca, if ob != 0 { ob } else { boundary })
                        }
                        None => (
                            add_weight.min(crate::temp_words::TEMP_WORD_MAX_WEIGHT),
                            1,
                            now_secs(),
                            boundary,
                        ),
                    };
                    t.insert(key.as_str(), enc_val(w, c, ca, b).as_slice())?;
                    let old_b = existing.map(|(_, _, _, ob)| ob);
                    crate::abbrev_index::shift(
                        &mut txn.open_table(TEMP_ABBREV)?,
                        schema,
                        code,
                        text,
                        old_b,
                        b,
                    )?;
                    promoted = true;
                }
            }
            txn.commit()?;
            Ok(promoted)
        })
    }

    /// 某方案当前的草稿条数（含已过期未清理的）。供容量维护与实测调参。
    pub fn count_drafts(&self, schema: &str) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(DRAFT_WORDS)?;
            let mut n = 0usize;
            for item in t.range(scan.as_str()..)? {
                let (k, _) = item?;
                if !k.value().starts_with(&scan) {
                    break;
                }
                n += 1;
            }
            Ok(n)
        })
    }

    /// 清空某方案的全部草稿。换词库/换方案配置后调用——草稿的码是按旧词库算的，留着就是错码。
    pub fn clear_drafts(&self, schema: &str) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let mut deleted = 0usize;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                let mut doomed: Vec<String> = Vec::new();
                for item in t.range(scan.as_str()..)? {
                    let (k, _) = item?;
                    let key = k.value();
                    if !key.starts_with(&scan) {
                        break;
                    }
                    doomed.push(key.to_string());
                }
                for key in &doomed {
                    t.remove(key.as_str())?;
                    deleted += 1;
                }
            }
            txn.commit()?;
            Ok(deleted)
        })
    }
}

/// 拆 key 供调试/导出用（与 `user_words::split_key` 同一套编码）。
pub fn parse_draft_key(key: &str) -> Option<(&str, &str, &str)> {
    split_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    fn items(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(c, t)| (c.to_string(), t.to_string()))
            .collect()
    }

    /// 写入 → 精确召回；**前缀查不到**（草稿只精确命中，见 `search_drafts` 的文档）。
    #[test]
    fn drafts_are_found_by_exact_code_only() {
        let p = tmp("wind_draft_exact.redb");
        let s = Store::open(&p).unwrap();
        s.add_drafts("wb", &items(&[("wqvb", "你好"), ("wqv", "你女")]))
            .unwrap();
        assert_eq!(s.search_drafts("wb", "wqvb", 0).unwrap(), vec!["你好"]);
        assert_eq!(s.search_drafts("wb", "wqv", 0).unwrap(), vec!["你女"]);
        // 不是任何一条的完整码 ⇒ 什么都不该出来（前缀不召回）
        assert!(s.search_drafts("wb", "wq", 0).unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    /// **过期判定必须在查询里**，不能只靠定期清理——否则清理没跑到的窗口里过期草稿照样被召回。
    ///
    /// 直接写表构造隔天的 `created_at`：`add_drafts` 取的是当下秒数，公共 API 造不出这个场景。
    #[test]
    fn expired_drafts_are_filtered_at_query_time_not_only_by_purge() {
        let p = tmp("wind_draft_ttl.redb");
        let s = Store::open(&p).unwrap();
        let now = now_secs();
        s.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                for (code, text, ca) in [
                    ("aaaa", "刚记的", now - 10),
                    ("bbbb", "昨天的", now - 90_000), // > 1 天
                ] {
                    let key = enc_key("wb", code, text);
                    t.insert(key.as_str(), enc_draft(ca).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
        let day = 86_400;
        assert_eq!(s.search_drafts("wb", "aaaa", day).unwrap(), vec!["刚记的"]);
        assert!(
            s.search_drafts("wb", "bbbb", day).unwrap().is_empty(),
            "过期草稿必须在查询时就被滤掉，不能等清理"
        );
        // ttl=0 ＝ 永不过期
        assert_eq!(s.search_drafts("wb", "bbbb", 0).unwrap(), vec!["昨天的"]);
        // 清理只删过期的那条
        assert_eq!(s.purge_expired_drafts("wb", day).unwrap(), 1);
        assert_eq!(s.count_drafts("wb").unwrap(), 1);
        let _ = std::fs::remove_file(&p);
    }

    /// 再次写入同一条草稿要**续期**，于是容量淘汰是近似 LRU 而非纯 FIFO。
    #[test]
    fn rewriting_a_draft_renews_it_so_eviction_is_lru_like() {
        let p = tmp("wind_draft_renew.redb");
        let s = Store::open(&p).unwrap();
        let now = now_secs();
        // 三条，created_at 递增；其中最老的那条稍后被「再打一次」
        s.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(DRAFT_WORDS)?;
                for (code, text, ca) in [
                    ("aaaa", "最老", now - 300),
                    ("bbbb", "中间", now - 200),
                    ("cccc", "最新", now - 100),
                ] {
                    let key = enc_key("wb", code, text);
                    t.insert(key.as_str(), enc_draft(ca).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
        // 「最老」被再次打出 ⇒ 续期到现在
        s.add_drafts("wb", &items(&[("aaaa", "最老")])).unwrap();
        // 保留 2 条 ⇒ 该淘汰的是「中间」（现在它才是 created_at 最小的）
        assert_eq!(s.evict_drafts("wb", 2).unwrap(), 1);
        assert!(
            !s.search_drafts("wb", "aaaa", 0).unwrap().is_empty(),
            "续期过的草稿不该被当成最老的淘汰掉"
        );
        assert!(s.search_drafts("wb", "bbbb", 0).unwrap().is_empty());
        assert!(!s.search_drafts("wb", "cccc", 0).unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    /// 跃迁：草稿消失、临时词出现，且**简拼索引必须跟着建**。
    #[test]
    fn promoting_a_draft_moves_it_and_maintains_the_abbrev_index() {
        let p = tmp("wind_draft_promote.redb");
        let s = Store::open(&p).unwrap();
        s.add_drafts("py", &items(&[("nihao", "你好")])).unwrap();
        assert!(
            s.promote_draft_to_temp("py", "nihao", "你好", 800, 0b101)
                .unwrap()
        );
        assert!(
            s.search_drafts("py", "nihao", 0).unwrap().is_empty(),
            "跃迁后草稿该消失"
        );
        let temp = s.get_temp_words("py", "nihao").unwrap();
        assert_eq!(temp.len(), 1);
        assert_eq!(temp[0].text, "你好");
        assert_eq!(temp[0].count, 1);
        assert!(
            !s.search_temp_words_by_abbrev("py", "nh", 0)
                .unwrap()
                .is_empty(),
            "主表写了索引没写 ⇒ 这个词的简拼静默召不回"
        );
        // 不存在的草稿：什么都不做
        assert!(
            !s.promote_draft_to_temp("py", "nihao", "你好", 800, 0b101)
                .unwrap()
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 跃迁到一条**已存在**的临时词上：count++，不覆盖它的 weight 与历史。
    #[test]
    fn promoting_onto_an_existing_temp_word_bumps_count_without_erasing_history() {
        let p = tmp("wind_draft_promote_existing.redb");
        let s = Store::open(&p).unwrap();
        s.learn_temp_word("wb", "wqvb", "你好", 1234, 0).unwrap(); // 已有、权重更高
        s.add_drafts("wb", &items(&[("wqvb", "你好")])).unwrap();
        assert!(
            s.promote_draft_to_temp("wb", "wqvb", "你好", 800, 0)
                .unwrap()
        );
        let temp = s.get_temp_words("wb", "wqvb").unwrap();
        assert_eq!(temp.len(), 1);
        assert_eq!(temp[0].count, 2, "该是 count++ 而不是重置");
        assert_eq!(
            temp[0].weight, 1234,
            "草稿没有资格把用户用过的词的权重压下去"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 方案隔离：一个方案的草稿不该被另一个方案看见或误删。
    #[test]
    fn drafts_are_isolated_per_schema() {
        let p = tmp("wind_draft_schema.redb");
        let s = Store::open(&p).unwrap();
        s.add_drafts("wb", &items(&[("aaaa", "五笔的")])).unwrap();
        s.add_drafts("py", &items(&[("aaaa", "拼音的")])).unwrap();
        assert_eq!(s.search_drafts("wb", "aaaa", 0).unwrap(), vec!["五笔的"]);
        assert_eq!(s.clear_drafts("wb").unwrap(), 1);
        assert_eq!(
            s.search_drafts("py", "aaaa", 0).unwrap(),
            vec!["拼音的"],
            "清空一个方案不该动到另一个"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_batch_is_a_noop() {
        let p = tmp("wind_draft_empty.redb");
        let s = Store::open(&p).unwrap();
        assert_eq!(s.add_drafts("wb", &[]).unwrap(), 0);
        assert_eq!(s.count_drafts("wb").unwrap(), 0);
        let _ = std::fs::remove_file(&p);
    }
}
