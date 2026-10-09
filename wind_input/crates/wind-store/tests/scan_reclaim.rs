//! 全表扫描之后的短档回收（`docs/design/memory-footprint.md` §6）。
//!
//! 扫描做完只置「有扫描待回收」标志，库空闲一小段（生产 3 秒）后由回收线程丢一次：
//! - 请求驱动扫描（设置页翻页）：[`Store::mark_scan_pending`]——连续翻页不逐页回收；
//! - 后台扫描（建索引）：[`Store::mark_scan_pending_rows`]，带行数门槛（小库不标记）——
//!   打字中自动造词反复触发重建，也只在停手之后回收一次。
//!
//! 判据同 `idle_cache_reclaim.rs`：落在 `page_cache_drops()` 与钩子调用次数，不看 RSS。
//! 时间参数取毫秒级；长档一律给到一小时，保证观察到的回收只可能来自被测的那一档。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use wind_store::Store;
use wind_store::store::SCAN_RECLAIM_MIN_ROWS;

const TICK: Duration = Duration::from_millis(20);
const SCAN_IDLE: Duration = Duration::from_millis(100); // = 5 拍
const NEVER: Duration = Duration::from_secs(3600);

fn store(tag: &str) -> (Arc<Store>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("wind_scanrc_{}_{tag}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    (Arc::new(Store::open(&p).expect("开库")), p)
}

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

fn counting_hook() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    (calls, move || {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

/// ★ 后台扫描那档：大扫描建完**不立即**回收，空闲满短档后回收一次并调钩子。
#[test]
fn large_background_scan_is_reclaimed_after_a_short_idle_not_at_once() {
    let (s, p) = store("bg");
    let (calls, hook) = counting_hook();
    s.spawn_idle_cache_reclaimer_then(NEVER, SCAN_IDLE, TICK, hook);

    s.count_user_words("py").expect("扫");
    s.mark_scan_pending_rows(SCAN_RECLAIM_MIN_ROWS);
    assert_eq!(s.page_cache_drops(), 0, "不在建索引线程上当场关库重开");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    assert!(wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1));
    assert!(wait_until(SCAN_IDLE * 5, || calls.load(Ordering::SeqCst) >= 1));
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(s.page_cache_drops(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "钩子恰好调一次");
    assert_eq!(s.count_user_words("py").expect("回收后照常可用"), 0);
    let _ = std::fs::remove_file(&p);
}

/// ★ 小库扫完不标记（扫进缓存的那点页不值得一次关库重开），短档不生效；留给长档。
#[test]
fn small_background_scan_is_not_marked() {
    let (s, p) = store("small");
    let (calls, hook) = counting_hook();
    s.spawn_idle_cache_reclaimer_then(NEVER, SCAN_IDLE, TICK, hook);
    s.count_user_words("py").expect("扫");
    s.mark_scan_pending_rows(SCAN_RECLAIM_MIN_ROWS - 1);
    std::thread::sleep(SCAN_IDLE * 4);
    assert_eq!(s.page_cache_drops(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_file(&p);
}

/// ★ 模拟打字：持续访问期间后台扫描反复标记——一次都不回收；停手满短档后只回收一次。
#[test]
fn repeated_background_scans_while_typing_reclaim_once_after_typing_stops() {
    let (s, p) = store("typing");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    let t0 = std::time::Instant::now();
    let mut i = 0;
    while t0.elapsed() < SCAN_IDLE * 6 {
        let _ = s.count_user_words("py").expect("按键查库");
        if i % 10 == 0 {
            s.mark_scan_pending_rows(SCAN_RECLAIM_MIN_ROWS); // 自动造词 ⇒ 重建 ⇒ 标记
        }
        i += 1;
        std::thread::sleep(TICK / 4);
    }
    assert_eq!(s.page_cache_drops(), 0, "持续打字期间不该回收");
    assert!(wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1));
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(s.page_cache_drops(), 1, "停手后只回收一次");
    let _ = std::fs::remove_file(&p);
}

/// ★ 请求驱动那档：标记之后不立刻丢；空闲满短档丢一次并调钩子；之后不再重复。
#[test]
fn marked_scan_is_reclaimed_once_after_a_short_idle() {
    let (s, p) = store("marked");
    let (calls, hook) = counting_hook();
    s.spawn_idle_cache_reclaimer_then(NEVER, SCAN_IDLE, TICK, hook);

    s.count_user_words("py").expect("扫");
    s.mark_scan_pending();
    assert_eq!(s.page_cache_drops(), 0, "只标记，不立刻回收");

    assert!(
        wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1),
        "空闲 {SCAN_IDLE:?} 后该回收一次，实际 drops={}",
        s.page_cache_drops()
    );
    assert!(wait_until(SCAN_IDLE * 5, || calls.load(Ordering::SeqCst) >= 1));

    std::thread::sleep(SCAN_IDLE * 4);
    assert_eq!(s.page_cache_drops(), 1, "标志已清，不该重复回收");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_file(&p);
}

/// ★ 标记之后库一直有人在用（连续翻页 / 打字）就不许丢；停手后才丢一次。
#[test]
fn marked_scan_waits_while_the_store_is_busy() {
    let (s, p) = store("busy");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);

    s.count_user_words("py").expect("扫");
    s.mark_scan_pending();
    let t0 = std::time::Instant::now();
    while t0.elapsed() < SCAN_IDLE * 5 {
        let _ = s.count_user_words("py").expect("查");
        std::thread::sleep(TICK / 4);
    }
    assert_eq!(s.page_cache_drops(), 0, "持续访问期间不该回收");

    assert!(
        wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1),
        "停手后该回收"
    );
    let _ = std::fs::remove_file(&p);
}

/// ★ 没有标记就只走长档：越过短档时长也不丢，到长档才丢。
#[test]
fn without_a_mark_only_the_long_idle_applies() {
    const IDLE: Duration = Duration::from_millis(600); // = 30 拍
    let (s, p) = store("unmarked");
    s.spawn_idle_cache_reclaimer(IDLE, SCAN_IDLE, TICK);

    s.count_user_words("py").expect("扫");
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(s.page_cache_drops(), 0, "没标记，短档不该生效");

    assert!(
        wait_until(IDLE * 10, || s.page_cache_drops() >= 1),
        "长档照常回收"
    );
    let _ = std::fs::remove_file(&p);
}

/// 回收之后既没有访问也没有新标记 ⇒ 不空转（闲置的输入法不该反复重开数据库）。
#[test]
fn after_a_reclaim_without_access_or_mark_it_does_not_spin() {
    let (s, p) = store("spin");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);

    s.count_user_words("py").expect("扫");
    s.mark_scan_pending();
    assert!(wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1));

    std::thread::sleep(SCAN_IDLE * 4);
    assert_eq!(s.page_cache_drops(), 1, "回收后没人碰过，不该再丢");
    let _ = std::fs::remove_file(&p);
}

/// ★ 标记本身就意味着刚访问过（扫描刚做完）：回收之后单独一次标记也要在短档后回收。
#[test]
fn a_mark_alone_counts_as_access() {
    let (s, p) = store("markalone");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    s.count_user_words("py").expect("扫");
    s.mark_scan_pending();
    assert!(wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1));

    s.mark_scan_pending_rows(SCAN_RECLAIM_MIN_ROWS);
    assert!(
        wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 2),
        "标记即访问，应再回收一次"
    );
    let _ = std::fs::remove_file(&p);
}

/// ★ 一次很长的 `with_db`（单事务导入、冷扫描）期间不许回收；空闲要从它**结束**时算起——
/// 否则它一结束回收线程就立刻关库重开（还会与批量写之后的立即回收撞在一起）。
#[test]
fn a_long_operation_is_not_idle_time() {
    let (s, p) = store("long");
    s.add_user_word("py", "ni", "你", 1, 0).expect("加词");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    s.mark_scan_pending();
    // `for_each_user_word` 的回调跑在 `with_db` 里面：在里面睡过好几个短档。
    s.for_each_user_word("py", "", &mut |_| {
        std::thread::sleep(SCAN_IDLE * 4);
        true
    })
    .expect("长操作");
    std::thread::sleep(SCAN_IDLE / 2);
    assert_eq!(
        s.page_cache_drops(),
        0,
        "长操作刚结束、还没空闲满短档，不该回收"
    );
    assert!(
        wait_until(SCAN_IDLE * 20, || s.page_cache_drops() >= 1),
        "结束后空闲满短档才回收"
    );
    let _ = std::fs::remove_file(&p);
}

/// ★ 暂停态（`pause`）没有缓存可丢：回收线程不许算一次回收、不许调钩子。
#[test]
fn a_paused_store_is_not_reclaimed_and_the_hook_is_not_run() {
    let (s, p) = store("paused");
    let (calls, hook) = counting_hook();
    s.spawn_idle_cache_reclaimer_then(NEVER, SCAN_IDLE, TICK, hook);
    s.count_user_words("py").expect("扫");
    s.pause().expect("暂停");
    s.mark_scan_pending();
    std::thread::sleep(SCAN_IDLE * 5);
    assert_eq!(s.page_cache_drops(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "暂停态不该整理堆");
    s.resume().expect("恢复");
    let _ = std::fs::remove_file(&p);
}

/// 闲置时回收线程按慢拍（`tick` 的 10 倍）睡；这时来的标记要把它叫醒，短档照常生效，
/// 不被慢拍拖到十拍之后。
#[test]
fn a_mark_wakes_an_idle_reclaimer() {
    const SLOW_TICK: Duration = Duration::from_millis(50); // 闲置慢拍 = 500 ms
    let (s, p) = store("wake");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, SLOW_TICK);
    s.count_user_words("py").expect("扫");
    s.mark_scan_pending();
    assert!(wait_until(Duration::from_secs(5), || s.page_cache_drops() >= 1));
    // 回收之后闲置：线程进入慢拍。等它睡下。
    std::thread::sleep(SLOW_TICK * 3);

    let t0 = std::time::Instant::now();
    s.count_user_words("py").expect("再扫");
    s.mark_scan_pending();
    assert!(wait_until(Duration::from_secs(5), || s.page_cache_drops() >= 2));
    let took = t0.elapsed();
    assert!(
        took < SCAN_IDLE + SLOW_TICK * 5,
        "标记应叫醒慢拍中的回收线程：用了 {took:?}"
    );
    let _ = std::fs::remove_file(&p);
}
