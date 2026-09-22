//! 计数 API 与列表 API 的**口径必须逐条一致**。
//!
//! 设置页用 `count_*` 显示「共 N 条」，用 `search_*_prefix` / `for_each_*` 取实际行。
//! 两边各写一遍「哪些记录算数」，迟早分叉——表现是「说有 N 条，翻到最后一页只有 N-1 条」，
//! 而且只在库里有解不出的坏记录时才出现，正常测试一辈子碰不到。
//!
//! 所以这里不比某个具体数字，只比**两条路彼此相等**。
//!
//! # ⚠️ 这套比对本身守不住口径分叉，真正的保证在结构里
//!
//! 在**全是好记录**的库上比两条路相等，测不出坏记录上的分叉——把某个 `count_*` 的过滤
//! 条件删成无条件 `n += 1`，这里照样绿（2026-09-22 审查实测，当时 `count_temp_words`
//! 正是一份独立抄写的实现）。要让它成真护栏，得往表里插一条畸形 value，而那几张表是
//! `pub(crate)`，集成测试够不着。
//!
//! 既然测不住，就改成**结构上不可能分叉**：`count_user_words` / `count_temp_words` 与各自
//! 的 `search_*` 全部 delegate 到同一个 `for_each_*`，「哪些记录算数」只有一份判据。
//! 本文件退到粗筛——它仍能抓住「delegate 链被改断」「跨方案串数」「流式与物化不同序」
//! 这些，但别指望它守口径。

use wind_store::{Store, wdict::WordIo};

fn store() -> (Store, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!(
        "wind_counts_{}_{}.redb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    (Store::open(&p).expect("开库"), p)
}

#[test]
fn user_word_count_matches_the_listing() {
    let (s, p) = store();
    let rows: Vec<WordIo> = (0..200)
        .map(|i| WordIo {
            code: format!("ni hao{i}"),
            text: format!("你好{i}"),
            weight: 100,
            count: 0,
            boundary: None,
        })
        .collect();
    s.import_user_words("py", &rows).expect("导入");
    // 另一个方案的词不得被数进来。
    s.import_user_words("wb", &rows[..7]).expect("导入");

    let listed = s.search_user_words_prefix("py", "", 0).expect("列表").len();
    assert_eq!(s.count_user_words("py").expect("计数"), listed);
    assert_eq!(listed, 200, "前提：200 条应全在");
    assert_eq!(s.count_user_words("wb").expect("计数"), 7, "不得跨方案串数");
    assert_eq!(s.count_user_words("nope").expect("计数"), 0);

    let _ = std::fs::remove_file(&p);
}

#[test]
fn temp_word_count_matches_the_listing() {
    let (s, p) = store();
    for i in 0..37 {
        s.learn_temp_word("py", &format!("ce{i}"), &format!("词{i}"), 100, 0)
            .expect("造临时词");
    }
    let listed = s.search_temp_words_prefix("py", "", 0).expect("列表").len();
    assert_eq!(s.count_temp_words("py").expect("计数"), listed);
    assert_eq!(listed, 37, "前提：37 条应全在");
    assert_eq!(s.count_temp_words("wb").expect("计数"), 0, "不得跨方案串数");

    let _ = std::fs::remove_file(&p);
}

/// ★ 流式遍历看到的，必须与一次性物化的列表**逐条相同**（顺序也一样）。
///
/// 这条守的是「`for_each_user_word` 能不能安全地替换 `search_user_words_prefix`」——
/// 设置页的分页要靠它只物化一页，前提是它俩走的是同一条扫描、同一套过滤。
#[test]
fn the_streaming_walk_sees_exactly_what_the_listing_does() {
    let (s, p) = store();
    let rows: Vec<WordIo> = (0..150)
        .map(|i| WordIo {
            code: format!("a{i:03}"),
            text: format!("词{i:03}"),
            weight: (i % 7) as i32,
            count: 0,
            boundary: None,
        })
        .collect();
    s.import_user_words("py", &rows).expect("导入");

    let listed = s.search_user_words_prefix("py", "", 0).expect("列表");
    let mut walked = Vec::new();
    s.for_each_user_word("py", "", &mut |w| {
        walked.push(w.to_record());
        true
    })
    .expect("遍历");
    assert_eq!(walked, listed, "流式遍历与列表必须逐条一致（含顺序）");

    // 前缀过滤同样一致。
    let listed = s.search_user_words_prefix("py", "a01", 0).expect("列表");
    let mut walked = Vec::new();
    s.for_each_user_word("py", "a01", &mut |w| {
        walked.push(w.to_record());
        true
    })
    .expect("遍历");
    assert_eq!(walked, listed);
    assert!(!listed.is_empty(), "前提：a01* 应有命中");

    // 回调返 false 即停——分页早退靠它。
    let mut seen = 0usize;
    s.for_each_user_word("py", "", &mut |_| {
        seen += 1;
        seen < 10
    })
    .expect("遍历");
    assert_eq!(seen, 10, "返回 false 之后不得继续扫");

    let _ = std::fs::remove_file(&p);
}
