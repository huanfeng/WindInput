//! redb 读缓存的**高水位**行为：全表读之后它涨上去，就不会再落下来。
//!
//! 读缓存的性质（堆上页、只增不减、脏页晋升，上界 `min(读缓存配额, 被读到过的页字节)`）
//! 与回收手法见 `docs/design/memory-footprint.md` §6.1。配额是 `DEFAULT_CACHE_SIZE_BYTES`
//! 的 90%（redb 2.x 按 9:1 切读/写）。
//!
//! 别拿 RSS 降不降当「释放了」的判据：glibc 不把 4 KiB 小块还给 OS，那是 `evict` / `hold`
//! 两组用同尺寸探针去证的。必须一个阶段一个进程：同进程里前一阶段的分配器保留量会盖住
//! 差值。
//!
//! 手动跑（顺序不能换）：
//! ```text
//! cd wind_input
//! M=--ignored\ --nocapture; T="cargo test -p wind-store --release --test redb_cache_high_water"
//! WIND_HW=build  $T -- $M    # 建库并导入 19 万条
//! WIND_HW=import $T -- $M    # 导入 + 验证 pause/resume 能否把缓存要回来
//! WIND_HW=idle   $T -- $M    # 只开库，不读 → 基线
//! WIND_HW=scan   $T -- $M    # 开库 + 全表扫 → 差值就是读缓存
//! WIND_HW=trim   $T -- $M    # 全表扫 → 丢缓存 → malloc_trim，看 RSS 能否落回
//! WIND_HW=scanreclaim $T -- $M  # 全表扫一遍 → 标记待回收 → 等过短档（钩子 malloc_trim）
//! ```

use wind_store::Store;
use wind_store::completion::CompletionKind;

/// 负载用旧补全历史表（`legacy_*` 写读器直接走 redb）：P2–P4a 起运行期数据都不在 redb，页缓存
/// 机制要拿仍能读写的 redb 表来测；P4b 随 redb 一起删。
const K: CompletionKind = CompletionKind::UrlHistory;

const N: usize = 190_000;

fn db_path() -> std::path::PathBuf {
    std::env::temp_dir().join("wind_redb_high_water.redb")
}

/// 本进程常驻集（MB）。Linux 专用；本用例只在本机手动跑。
fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    pages * 4096.0 / 1024.0 / 1024.0
}

/// 2~4 音节轮换，贴近真实拼音词库的键分布；换成等长串会让页填充率和库大小失真。
fn legacy_rows(data: &[(String, String)]) -> Vec<(CompletionKind, String, u32, i64)> {
    data.iter()
        .map(|(code, _)| (K, code.clone(), 1, 0))
        .collect()
}

fn rows() -> Vec<(String, String)> {
    let l = b"abcdefghijklmnopqrstuvwxyz";
    (0..N)
        .map(|i| {
            let syl = 2 + (i % 3);
            let mut x = i;
            let mut segs: Vec<String> = Vec::with_capacity(syl);
            for _ in 0..syl {
                segs.push(format!("{}i", l[x % 26] as char));
                x /= 26;
            }
            (segs.concat(), format!("词{i}"))
        })
        .collect()
}

#[test]
#[ignore = "观测，不参与常规回归；须按 WIND_HW=build/idle/scan/import 分进程跑"]
fn the_page_cache_is_a_high_water_mark() {
    let db = db_path();
    match std::env::var("WIND_HW").as_deref() {
        Ok("build") => {
            let _ = std::fs::remove_file(&db);
            let data = rows();
            let s = Store::open(&db).expect("开库");
            s.legacy_put_completions(&legacy_rows(&data)).expect("导入");
            let mb = std::fs::metadata(&db)
                .map(|m| m.len() as f64 / 1048576.0)
                .unwrap();
            println!("[build] 导入 {N} 条，库文件 {mb:.1} MB");
        }
        Ok("idle") => {
            let before = rss_mb();
            let _s = Store::open(&db).expect("开库");
            println!(
                "[idle]  只开库不读       RSS {:.1} → {:.1} MB",
                before,
                rss_mb()
            );
        }
        // 导入这一趟：脏页晋升 + 索引回查，两头都在填读缓存。
        // 末尾用「关库重开」验证这笔内存能不能要回来，即 `Store::pause` + `resume` 做的事。
        Ok("import") => {
            let _ = std::fs::remove_file(&db);
            let data = rows();
            let ready = rss_mb();
            let s = Store::open(&db).expect("开库");
            s.legacy_put_completions(&legacy_rows(&data)).expect("导入");
            let imported = rss_mb();
            drop(data);
            let dropped = rss_mb();
            let mb = std::fs::metadata(&db)
                .map(|m| m.len() as f64 / 1048576.0)
                .unwrap();
            println!("[import] 入参就绪           RSS {ready:.1} MB");
            println!("[import] 导入完成           RSS {imported:.1} MB  (库 {mb:.1} MB)");
            println!("[import] 丢掉入参           RSS {dropped:.1} MB  ← 还没动缓存");
            // 关库重开 = 丢掉整个 PagedCachedFile。
            s.pause().expect("暂停");
            let paused = rss_mb();
            s.resume().expect("恢复");
            let resumed = rss_mb();
            println!("[import] pause（丢弃 db）   RSS {paused:.1} MB  ← 缓存在这一步被释放");
            println!("[import] resume（重开）     RSS {resumed:.1} MB");
            let n = s.legacy_scan_completions(K, "ai", 30).expect("查").len();
            println!("[import] 重开后仍可查（{n} 条）RSS {:.1} MB", rss_mb());
        }
        Ok("scan") => {
            let before = rss_mb();
            let s = Store::open(&db).expect("开库");
            let opened = rss_mb();
            let n = s.legacy_scan_completions(K, "", 0).expect("全表扫").len();
            let scanned = rss_mb();
            // 结果立刻丢弃：留下的只可能是缓存，不是结果本身。
            println!("[scan]  开库             RSS {before:.1} → {opened:.1} MB");
            println!("[scan]  全表扫 {n} 条  RSS {opened:.1} → {scanned:.1} MB");
            for i in 0..3 {
                let _ = s.legacy_scan_completions(K, "", 0).expect("再扫");
                println!("[scan]  再扫第 {} 次       RSS {:.1} MB", i + 1, rss_mb());
            }
            println!("[scan]  ↑ 只涨不落即为高水位");
        }
        // 「缓存到底有没有被释放」的判据：不看 RSS（glibc 不把 4 KiB 小块还给 OS），而看
        // 释放出来的空间能否被后来的分配复用——填满缓存后再要 PROBE MB，RSS 几乎不涨即已释放。
        // `WIND_HW=hold` 是对照组（不释放，RSS 必须涨满）。
        Ok(mode @ ("evict" | "hold")) => {
            const PROBE: usize = 40 * 1024 * 1024;
            let s = Store::open(&db).expect("开库");
            for _ in 0..3 {
                let _ = s.legacy_scan_completions(K, "", 0).expect("全表扫");
            }
            let filled = rss_mb();
            if mode == "evict" {
                s.pause().expect("暂停");
            }
            let after = rss_mb();
            // 探针必须与 redb 的页同尺寸（4 KiB 的许多小块）：超过 `M_MMAP_THRESHOLD`
            // （默认 128 KiB）的大分配直接走 `mmap`、不看 free list，两组都会涨满。写满才触达物理页。
            const PAGE: usize = 4096;
            let mut probe: Vec<Vec<u8>> = (0..PROBE / PAGE).map(|_| vec![1u8; PAGE]).collect();
            probe.iter_mut().for_each(|b| b[0] = 2);
            std::hint::black_box(&probe);
            let probed = rss_mb();
            println!(
                "[{mode}] 缓存填满 {filled:.1} → {}{after:.1} → 再要 {} MB 后 {probed:.1}  (涨了 {:.1})",
                if mode == "evict" {
                    "pause 后 "
                } else {
                    "不释放 "
                },
                PROBE / 1048576,
                probed - after
            );
            if mode == "evict" {
                s.resume().expect("恢复");
                let n = s.legacy_scan_completions(K, "ai", 30).expect("查").len();
                println!("[evict] resume 后仍可查（{n} 条）");
            }
        }
        // 空闲回收 + 整理堆（协调器 `heap_trim.rs` 接在 reclaimer 钩子上）能不能让 RSS
        // 真正落回。上面 evict 证的是「释放了」，这里证「还给 OS 了」。
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        Ok("trim") => {
            unsafe extern "C" {
                fn malloc_trim(pad: usize) -> i32;
            }
            let s = Store::open(&db).expect("开库");
            let opened = rss_mb();
            for _ in 0..3 {
                let _ = s.legacy_scan_completions(K, "", 0).expect("全表扫");
            }
            let filled = rss_mb();
            s.drop_page_cache().expect("回收");
            let dropped = rss_mb();
            unsafe { malloc_trim(0) };
            let trimmed = rss_mb();
            println!(
                "[trim] 开库 {opened:.1} → 全表扫 {filled:.1} → 丢缓存 {dropped:.1} → malloc_trim {trimmed:.1} MB"
            );
        }
        // 后台扫描那档：全表扫一遍 → `mark_scan_pending_rows` → 等过短档，由回收线程丢缓存并调
        // 钩子 malloc_trim（与协调器的 `heap_trim::release_free_heap` 同形）。与 `trim` 的差别：
        // 只扫一遍，回收走生产路径而不是手调两步。
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        Ok("scanreclaim") => {
            use std::time::Duration;
            unsafe extern "C" {
                fn malloc_trim(pad: usize) -> i32;
            }
            const SCAN_IDLE: Duration = Duration::from_millis(300);
            let before = rss_mb();
            let s = std::sync::Arc::new(Store::open(&db).expect("开库"));
            s.spawn_idle_cache_reclaimer_then(
                Duration::from_secs(3600),
                SCAN_IDLE,
                Duration::from_millis(50),
                || unsafe {
                    malloc_trim(0);
                },
            );
            let opened = rss_mb();
            let n = s.legacy_scan_completions(K, "", 0).expect("全表扫").len();
            let scanned = rss_mb();
            if n < wind_store::store::SCAN_RECLAIM_MIN_ROWS {
                println!(
                    "[scanreclaim] 未过门槛：只有 {n} 条（门槛 {}），不会标记；先跑 WIND_HW=build",
                    wind_store::store::SCAN_RECLAIM_MIN_ROWS
                );
                return;
            }
            s.mark_scan_pending_rows(n);
            let marked = rss_mb();
            let t0 = std::time::Instant::now();
            while s.page_cache_drops() == 0 && t0.elapsed() < SCAN_IDLE * 20 {
                std::thread::sleep(Duration::from_millis(10));
            }
            // 回收与钩子在同一拍里先后执行：drops 变了之后再给钩子一拍的时间。
            std::thread::sleep(Duration::from_millis(100));
            let reclaimed = rss_mb();
            let again = s
                .legacy_scan_completions(K, "", 0)
                .expect("回收后再查")
                .len();
            println!(
                "[scanreclaim] 开库 {before:.1} → {opened:.1} → 全表扫 {n} 条 {scanned:.1} → \
                 标记后 {marked:.1} → 空闲 {:?} 后回收（含 malloc_trim）{reclaimed:.1} MB；\
                 drops={}，回收后再数 {again} 条",
                t0.elapsed(),
                s.page_cache_drops()
            );
        }
        _ => println!(
            "请设 WIND_HW=build|idle|scan|import|evict|hold|trim|scanreclaim，见文件头的跑法"
        ),
    }
}
