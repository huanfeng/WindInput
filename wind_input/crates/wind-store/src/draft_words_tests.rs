use super::*;
use now_for_test as now_secs;

/// 以给定时刻写草稿（模拟「很久以前写的」）：P1 起草稿在运行时引擎，有效期基准是 last_used。
fn drafts_at(s: &Store, rows: &[(&str, &str, &str, i64)]) {
    use wind_userdata::engine::Op;
    use wind_userdata::record::{OpKind, Payload, Target};
    let ops: Vec<Op> = rows
        .iter()
        .map(|(schema, code, text, ca)| {
            let mut op = Op::new(
                Target::Draft,
                schema,
                OpKind::Upsert,
                code,
                text,
                Payload::Weight(0),
                0,
            );
            op.ts_ms = ca * 1000;
            op
        })
        .collect();
    s.runtime().data().apply_batch(&ops).unwrap();
}

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
/// 用带时刻的操作构造隔天的草稿：`add_drafts` 取的是当下秒数，公共 API 造不出这个场景。
#[test]
fn expired_drafts_are_filtered_at_query_time_not_only_by_purge() {
    let p = tmp("wind_draft_ttl.redb");
    let s = Store::open(&p).unwrap();
    let now = now_secs();
    drafts_at(
        &s,
        &[
            ("wb", "aaaa", "刚记的", now - 10),
            ("wb", "bbbb", "昨天的", now - 90_000), // > 1 天
        ],
    );
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
    drafts_at(
        &s,
        &[
            ("wb", "aaaa", "最老", now - 300),
            ("wb", "bbbb", "中间", now - 200),
            ("wb", "cccc", "最新", now - 100),
        ],
    );
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

/// 启动清理要**跨方案**：按方案清永远够不着「用户已经不用了的方案」留下的草稿。
///
/// 这正是加这一条通路的理由——落库后的清理只会带上当前方案的前缀。
#[test]
fn startup_purge_sweeps_every_schema_not_just_the_active_one() {
    let p = tmp("wind_draft_purge_all.redb");
    let s = Store::open(&p).unwrap();
    let now = now_secs();
    drafts_at(
        &s,
        &[
            ("wb", "aaaa", "五笔新的", now - 10),
            ("wb", "bbbb", "五笔旧的", now - 90_000),
            ("py", "cccc", "拼音旧的", now - 90_000),
            ("zr", "dddd", "自然旧的", now - 90_000),
        ],
    );
    let day = 86_400;
    // 对照：按方案清只收得掉 wb 那一条，另外两个方案原封不动。
    assert_eq!(s.purge_expired_drafts("wb", day).unwrap(), 1);
    assert_eq!(s.count_drafts("py").unwrap(), 1);
    assert_eq!(s.count_drafts("zr").unwrap(), 1);
    // 全表清一次把它们都收掉，且没碰 wb 那条还没过期的。
    // 第二个返回值是**剩余**条数——真机重定 `draft_max_entries` 就靠它。
    assert_eq!(s.purge_all_expired_drafts(day).unwrap(), (2, 1));
    assert_eq!(s.count_drafts("py").unwrap(), 0);
    assert_eq!(s.count_drafts("zr").unwrap(), 0);
    assert_eq!(
        s.search_drafts("wb", "aaaa", day).unwrap(),
        vec!["五笔新的"],
        "没过期的草稿不该被启动清理带走"
    );
    // ttl=0 ＝ 永不过期 ⇒ 不扫描、什么都不删（剩余数随之无意义，恒 0）
    assert_eq!(s.purge_all_expired_drafts(0).unwrap(), (0, 0));
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
