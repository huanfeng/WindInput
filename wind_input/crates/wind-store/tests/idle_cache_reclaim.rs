//! 空闲回收线程的三条判据。
//!
//! 它兜的是「写完就丢」堵不住的那一路：设置页的分页要给出精确 `total` 就不能中途早退，
//! 照样扫完整表、把叶子页全填进读缓存；正常打字也在慢慢填。redb 没有按范围计数的 API
//! （`len()` 是 O(1) 但只给整表，而 key 是 `schema\0code\0text`、一张表混着所有方案），
//! 所以「不扫全表也能拿到 total」要自己维护计数器、写路径一多就会漏。与其每条路上堵，
//! 不如空闲时统一回收。
//!
//! 判据落在 `page_cache_drops()` 而不是 RSS：缓存有没有被回收在外部**不可观测**
//! （`pause` 之后 RSS 一动不动，见 `redb_cache_high_water.rs`），只能由内部报数。
//!
//! 时间参数取毫秒级，跑得完；生产是 60 秒 / 1 秒一拍（短档 3 秒见 `scan_reclaim.rs`）。

use std::sync::Arc;
use std::time::Duration;
use wind_store::Store;
use wind_store::completion::CompletionKind;

/// 负载用旧补全历史表（`legacy_*` 写读器直接走 redb）：P2–P4a 起运行期数据都不在 redb，页缓存
/// 机制要拿仍能读写的 redb 表来测；P4b 随 redb 一起删。
const K: CompletionKind = CompletionKind::UrlHistory;

const TICK: Duration = Duration::from_millis(20);
const IDLE: Duration = Duration::from_millis(100); // = 5 拍

/// 正向等待的上限：只断言「最终会发生」，不断言多快。
///
/// macOS CI 上从起线程到第一次回收实测要 ~1 s（慢拍 200 ms + 五拍静默 + 关库重开时要刷掉
/// 迁移刚写下的脏页；同一条路径在 `scan_reclaim.rs::a_mark_wakes_an_idle_reclaimer` 里
/// 量到 370~400 ms，注释记了其中 ~250 ms 与拍长无关）。原先按 `IDLE * 10`（1 s）给，正好
/// 卡在边界：2026-10-24 与 2026-10-30 的 macOS CI 各红过一次，红的还是不同的用例
/// （`a_never_used…` 与 `the_after_hook…` 的第一条等待），而 Linux 上同样的等待只要 ~0.3 s。
/// 放宽只影响「真的坏了」时多等几秒，不影响任何一条断言的判据。
const WAIT: Duration = Duration::from_secs(10);

fn store(tag: &str) -> (Arc<Store>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("wind_idle_{}_{tag}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    (Arc::new(Store::open(&p).expect("开库")), p)
}

/// 轮询等待，直到 `f` 成立或超时。比固定 sleep 稳：CI 上线程调度不保证准时。
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(TICK / 2);
    }
    false
}

/// ★ 访问过、然后空闲够久 ⇒ 缓存被回收。
#[test]
fn an_idle_store_gives_its_page_cache_back() {
    let (s, p) = store("reclaims");
    s.spawn_idle_cache_reclaimer(IDLE, IDLE, TICK);

    s.legacy_put_completions(&[(K, "ni".into(), 1, 0)])
        .expect("写");
    assert_eq!(s.page_cache_drops(), 0, "刚访问完不该立刻丢");

    assert!(
        wait_until(WAIT, || s.page_cache_drops() >= 1),
        "空闲 {IDLE:?} 之后该回收一次，实际 drops={}",
        s.page_cache_drops()
    );
    // 回收之后数据还在、还能用。
    assert_eq!(s.legacy_scan_completions(K, "", 0).expect("计数").len(), 1);

    let _ = std::fs::remove_file(&p);
}

/// ★ 一直有人在用就不许丢 —— 丢一次要重开 `Database`，打字中途做这个是白添延迟。
#[test]
fn a_busy_store_is_never_reclaimed() {
    let (s, p) = store("busy");
    s.spawn_idle_cache_reclaimer(IDLE, IDLE, TICK);

    // 持续访问，跨度远超空闲阈值。
    let t0 = std::time::Instant::now();
    while t0.elapsed() < IDLE * 4 {
        let _ = s.legacy_scan_completions(K, "", 0).expect("查").len();
        std::thread::sleep(TICK / 2);
    }
    assert_eq!(
        s.page_cache_drops(),
        0,
        "一直在访问的库不该被回收（会给按键路径添一次重开开销）"
    );

    let _ = std::fs::remove_file(&p);
}

/// ★ 从没人用过就不许空转 —— 否则一个开着没在打字的输入法每分钟白重开一次库。
///
/// 这条守的是 `pending` 那个标志。删掉它（改成只看 `quiet >= ticks_to_idle`）本用例即红。
#[test]
fn a_never_used_store_is_not_reclaimed_on_a_timer() {
    let (s, p) = store("never");
    s.spawn_idle_cache_reclaimer(IDLE, IDLE, TICK);

    // 开库本身会经 `with_db`（run_migrations / backfill），所以先让那一笔过去：
    // 等第一次回收发生，再看此后是否还继续空转。
    assert!(
        wait_until(WAIT, || s.page_cache_drops() >= 1),
        "前提：开库那一笔访问之后应回收一次"
    );
    let after_first = s.page_cache_drops();

    std::thread::sleep(IDLE * 4);
    assert_eq!(
        s.page_cache_drops(),
        after_first,
        "没有新访问就不该再回收 —— 空转会让闲置的输入法反复重开数据库"
    );

    let _ = std::fs::remove_file(&p);
}

/// 每次回收之后恰好调一次钩子（上层在这里整理堆）：调用次数始终等于回收次数。
#[test]
fn the_after_hook_runs_once_per_reclaim() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (s, p) = store("after_hook");
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    s.spawn_idle_cache_reclaimer_then(IDLE, IDLE, TICK, move || {
        c.fetch_add(1, Ordering::SeqCst);
    });

    // 开库那一笔访问引发第一次回收（同 a_never_used_store_is_not_reclaimed_on_a_timer）。
    assert!(wait_until(WAIT, || calls.load(Ordering::SeqCst) >= 1));
    s.legacy_scan_completions(K, "", 0)
        .expect("再访问一次")
        .len();
    assert!(
        wait_until(WAIT, || calls.load(Ordering::SeqCst) >= 2),
        "第二次回收后该再调一次"
    );
    std::thread::sleep(IDLE * 3);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "没有新访问就不该再调");
    assert_eq!(s.page_cache_drops(), 2);
    drop(s);
    let _ = std::fs::remove_file(&p);
}
