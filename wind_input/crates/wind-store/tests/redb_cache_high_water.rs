//! redb 读缓存的**高水位**行为：全表读之后它涨上去，就不会再落下来。
//!
//! 这条是「导入大词库之后内存占用变高」那类反馈的正主。它与派生缓存的峰值不是一回事——
//! 派生缓存的几百 MB 是**构建期**的，函数返回即还给系统（2026-09-22 靶机实测：重建雾凇
//! merged 缓存峰值 491 MB，4 秒后落回 35.9 MB）；而 redb 读缓存是**长期常驻**的：
//!
//! - 页是堆上的 `Arc<[u8]>`（redb 2.x 不是 mmap），全额计进进程私有内存；
//! - 每次读未命中就插入，**只有越过上限才淘汰**，唯一的整体清空是库文件扩容；
//! - 事务刷盘时脏页被直接晋升进读缓存，所以批量导入写过的页当场变常驻。
//!
//! 上界 = `min(读缓存配额, 被读到过的页字节)`，而后者随用户导入单调变大。
//! 配额是 `DEFAULT_CACHE_SIZE_BYTES` 的 90%（redb 2.x 按 9:1 切读/写）。
//!
//! # 实测结论（2026-09-22，本机 19 万条 / 库 32.6 MB）
//!
//! | 阶段 | RSS |
//! |---|---|
//! | 只开库不读 | 4.5 MB |
//! | 全表扫一次 | 31.1 MB |
//! | 再扫到顶 | 45.6 MB ← 不再涨，也不落 |
//!
//! **导入 / 全表扫之后，这笔内存在进程活着期间一直不还。** 唯一的释放手段是丢弃
//! `Database`（`Store::pause`），`WIND_HW=evict` 与 `hold` 两组证明了它真的释放：
//! 填满缓存后再要 40 MB 同尺寸小块，不释放的一组 RSS 涨 15.2 MB，`pause` 过的一组
//! **涨 0.0 MB**——那 40 MB 全部由释放出来的空间供给。
//!
//! ⚠️ 别拿 RSS 降不降当判据：`pause` 之后 RSS 一动不动（glibc 不把 4 KiB 小块还给 OS）。
//! 「释放了」与「还给 OS 了」是两件事，本用例只能证前者。
//!
//! # 为什么必须一个阶段一个进程
//!
//! 首版把三个阶段写在一个测试里，量出来的数字是废的：关库之后 RSS 一点没降——分配器
//! 不把空闲块还给 OS，于是「缓存涨了多少」被前一阶段的保留量盖住了。同进程里测不出
//! 这个差值，只能各起一个进程、各自从干净的堆开始。
//!
//! 手动跑（三步，顺序不能换）：
//! ```text
//! cd wind_input
//! M=--ignored\ --nocapture; T="cargo test -p wind-store --release --test redb_cache_high_water"
//! WIND_HW=build  $T -- $M    # 建库并导入 19 万条
//! WIND_HW=import $T -- $M    # 导入 + 验证 pause/resume 能否把缓存要回来
//! WIND_HW=idle   $T -- $M    # 只开库，不读 → 基线
//! WIND_HW=scan   $T -- $M    # 开库 + 全表扫 → 差值就是读缓存
//! ```

use wind_store::{Store, wdict::WordIo};

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

/// 与 `perf_cache_size.rs` 同款夹具：2~4 音节轮换，贴近真实拼音词库的键分布。
/// 换成等长串会让页的填充率失真，库大小跟着失真。
fn rows() -> Vec<WordIo> {
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
            WordIo {
                code: segs.join(" "),
                text: format!("词{i}"),
                weight: 100,
                count: 0,
                boundary: None,
            }
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
            s.import_user_words("py", &data).expect("导入");
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
        // 末尾用「关库重开」验证这笔内存能不能要回来——那正是 `Store::pause` + `resume`
        // 做的事（丢弃 `Database` ⇒ `PagedCachedFile` 一起 drop）。
        Ok("import") => {
            let _ = std::fs::remove_file(&db);
            let data = rows();
            let ready = rss_mb();
            let s = Store::open(&db).expect("开库");
            s.import_user_words("py", &data).expect("导入");
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
            let n = s
                .search_user_words_prefix("py", "ai", 30)
                .expect("查")
                .len();
            println!("[import] 重开后仍可查（{n} 条）RSS {:.1} MB", rss_mb());
        }
        Ok("scan") => {
            let before = rss_mb();
            let s = Store::open(&db).expect("开库");
            let opened = rss_mb();
            let n = s
                .search_user_words_prefix("py", "", 0)
                .expect("全表扫")
                .len();
            let scanned = rss_mb();
            // 结果立刻丢弃：留下的只可能是缓存，不是结果本身。
            println!("[scan]  开库             RSS {before:.1} → {opened:.1} MB");
            println!("[scan]  全表扫 {n} 条  RSS {opened:.1} → {scanned:.1} MB");
            for i in 0..3 {
                let _ = s.search_user_words_prefix("py", "", 0).expect("再扫");
                println!("[scan]  再扫第 {} 次       RSS {:.1} MB", i + 1, rss_mb());
            }
            println!("[scan]  ↑ 只涨不落即为高水位");
        }
        // ★ 「缓存到底有没有被释放」的判据。
        //
        // 不能看 RSS 降不降：Linux 下 glibc 不把空闲块还给 OS（redb 的页是 4 KiB 的
        // `Arc<[u8]>`，属于小块，走不到 `munmap` 那条路），drop 之后 RSS 一动不动。
        // 靶机上 Windows 的 491 MB 峰值能落回，是因为那里有大量 >512 KiB 的大块走
        // VirtualAlloc——两者不能互相外推。
        //
        // 换个问法就干净了：**释放出来的空间，后来的分配能不能用上**。
        // 填满缓存后再要 PROBE MB，若 RSS 几乎不涨，说明那块空间已经归还给分配器、
        // 被复用了 ⇒ 缓存确实被释放。`WIND_HW=hold` 是对照组（不释放，RSS 必须涨满）。
        Ok(mode @ ("evict" | "hold")) => {
            const PROBE: usize = 40 * 1024 * 1024;
            let s = Store::open(&db).expect("开库");
            for _ in 0..3 {
                let _ = s.search_user_words_prefix("py", "", 0).expect("全表扫");
            }
            let filled = rss_mb();
            if mode == "evict" {
                s.pause().expect("暂停");
            }
            let after = rss_mb();
            // ⚠️ 探针必须与 redb 的页**同尺寸**（4 KiB 的许多小块），不能是一次 40 MB
            // 的大分配：glibc 对超过 `M_MMAP_THRESHOLD`（默认 128 KiB）的请求直接走
            // `mmap`，压根不看 free list，于是两组都会老老实实涨满 40 MB，什么也证明不了
            // ——这正是本用例第一版的错。写满才触达物理页。
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
                let n = s
                    .search_user_words_prefix("py", "ai", 30)
                    .expect("查")
                    .len();
                println!("[evict] resume 后仍可查（{n} 条）");
            }
        }
        _ => println!("请设 WIND_HW=build|idle|scan|import|evict|hold，见文件头的跑法"),
    }
}
