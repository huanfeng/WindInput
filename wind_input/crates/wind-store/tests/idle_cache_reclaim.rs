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
//! 时间参数取毫秒级，跑得完；生产是 60 秒 / 10 秒一拍。

use std::sync::Arc;
use std::time::Duration;
use wind_store::{Store, wdict::WordIo};

const TICK: Duration = Duration::from_millis(20);
const IDLE: Duration = Duration::from_millis(100); // = 5 拍

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
    s.spawn_idle_cache_reclaimer(IDLE, TICK);

    s.import_user_words(
        "py",
        &[WordIo {
            code: "ni".into(),
            text: "你".into(),
            weight: 100,
            count: 0,
            boundary: None,
        }],
    )
    .expect("导入");
    assert_eq!(s.page_cache_drops(), 0, "刚访问完不该立刻丢");

    assert!(
        wait_until(IDLE * 10, || s.page_cache_drops() >= 1),
        "空闲 {IDLE:?} 之后该回收一次，实际 drops={}",
        s.page_cache_drops()
    );
    // 回收之后数据还在、还能用。
    assert_eq!(s.count_user_words("py").expect("计数"), 1);

    let _ = std::fs::remove_file(&p);
}

/// ★ 一直有人在用就不许丢 —— 丢一次要重开 `Database`，打字中途做这个是白添延迟。
#[test]
fn a_busy_store_is_never_reclaimed() {
    let (s, p) = store("busy");
    s.spawn_idle_cache_reclaimer(IDLE, TICK);

    // 持续访问，跨度远超空闲阈值。
    let t0 = std::time::Instant::now();
    while t0.elapsed() < IDLE * 4 {
        let _ = s.count_user_words("py").expect("查");
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
    s.spawn_idle_cache_reclaimer(IDLE, TICK);

    // 开库本身会经 `with_db`（run_migrations / backfill），所以先让那一笔过去：
    // 等第一次回收发生，再看此后是否还继续空转。
    assert!(
        wait_until(IDLE * 10, || s.page_cache_drops() >= 1),
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
