//! `Store::drop_page_cache` 的两条契约。
//!
//! 它存在的全部理由是「把 redb 那笔只涨不落的读缓存还回去，而**不产生可观测的暂停态**」。
//! 用 `pause()` + `resume()` 也能丢掉缓存，但那是两次独立取锁，中间有个 `db` 为 `None`
//! 的窗口——落在窗口里的查询会拿到 `store is paused`，按键线路上就是一次吞字。
//!
//! 所以这里测两件事：数据还在（没把库弄丢），以及并发查询一次都不失败。
//!
//! ⚠️ 但**「单 guard」这个设计本身这里守不住**——见
//! [`concurrent_reads_survive_a_cache_drop`] 的说明，那个窗口窄到不可检出。
//! 缓存到底有没有被释放，由 `tests/redb_cache_high_water.rs` 的 evict/hold 两组回答。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use wind_store::{Store, wdict::WordIo};

fn store(tag: &str) -> (Arc<Store>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("wind_dpc_{}_{tag}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    (Arc::new(Store::open(&p).expect("开库")), p)
}

fn rows(n: usize) -> Vec<WordIo> {
    (0..n)
        .map(|i| WordIo {
            code: format!("a{i:04}"),
            text: format!("词{i:04}"),
            weight: 100,
            count: 0,
            boundary: None,
        })
        .collect()
}

/// 丢缓存不得丢数据——它换掉的只是 `Database` 实例，库文件原封不动。
#[test]
fn dropping_the_cache_keeps_every_row() {
    let (s, p) = store("rows");
    s.import_user_words("py", &rows(500)).expect("导入");
    let before = s.search_user_words_prefix("py", "", 0).expect("扫");

    s.drop_page_cache().expect("丢缓存");

    let after = s.search_user_words_prefix("py", "", 0).expect("扫");
    assert_eq!(after, before, "丢缓存前后必须逐条相同");
    assert_eq!(after.len(), 500);
    assert!(!s.is_paused(), "结束时不得停在暂停态");

    // 还能继续写。
    s.add_user_word("py", "zz", "新词", 100, 0).expect("写");
    assert_eq!(s.count_user_words("py").expect("计数"), 501);

    let _ = std::fs::remove_file(&p);
}

/// 丢缓存期间并发查询不得失败。
///
/// ⚠️ **这条守不住「必须单 guard」这个设计**，别把它当那个的护栏。
///
/// 我试过让它守：把实现换成 `self.pause()?; self.resume()?;`（那版有真实窗口），跑 30 次
/// 循环、1000 次循环都是绿的。原因是窗口只有「`pause` 的 guard drop 到 `resume` 取锁」
/// 这一瞬，纳秒级，而 std 的 Mutex 不保证公平——同一线程连着调，读线程几乎不可能插进去。
/// 换句话说那个变异**实际上不可检出**，写多少次循环都一样。
///
/// 「同一个 guard 内换掉」的保证来自实现本身（`drop_page_cache` 全程只取一次锁），
/// 不来自这条测试。留着它是因为它仍能抓另一类真实回归：丢缓存时若做了别的事
/// （重建索引、迁移、IO 重试）而让查询报错或 panic，这里会红。
#[test]
fn concurrent_reads_survive_a_cache_drop() {
    let (s, p) = store("concurrent");
    s.import_user_words("py", &rows(2000)).expect("导入");

    let stop = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (s, stop, failures, reads) = (
                Arc::clone(&s),
                Arc::clone(&stop),
                Arc::clone(&failures),
                Arc::clone(&reads),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match s.search_user_words_prefix("py", "a01", 10) {
                        Ok(_) => reads.fetch_add(1, Ordering::Relaxed),
                        Err(_) => failures.fetch_add(1, Ordering::Relaxed),
                    };
                }
            })
        })
        .collect();

    for _ in 0..200 {
        s.drop_page_cache().expect("丢缓存");
    }
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("读线程");
    }

    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "前提：读线程得真的读到过东西"
    );
    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "丢缓存期间不得有任何查询失败"
    );

    let _ = std::fs::remove_file(&p);
}
