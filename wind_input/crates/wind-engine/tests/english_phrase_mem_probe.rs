//! 英文词组分词索引：堆版 vs 落盘 mmap 版的**常驻**与**耗时**探针（`#[ignore]`，手动跑）。
//!
//! ```text
//! cargo test -p wind-engine --release --test english_phrase_mem_probe -- --ignored --nocapture
//! ```
//!
//! 合成词库模仿靶机那份：18 万条词组（两种编码方案并存）+ 10 万条单词（全表扫要过它们，
//! 只是不进索引）。三档各跑 3 轮：
//!
//! - 堆版：`LazyPhraseIndex::new()`，每次全表扫、整张表常驻堆；
//! - 首建：带缓存目录、目录为空 ⇒ 全表扫 + 写盘 + 从盘上 mmap 打开；
//! - 命中：带缓存目录、文件已在 ⇒ 不扫，直接 mmap。
//!
//! 「常驻」取两种口径：计数分配器的 live 字节差（只算本进程堆，精确）与 `/proc/self/status`
//! 的 `RssAnon` 差（匿名页，Linux 限定，含分配器碎片）。mmap 的文件页不计入两者——
//! 那正是要的效果：它们可被系统按需换出、多进程共享，不是进程私有内存。
//!
//! ⚠️ `RssAnon` 必须**一档一进程**量：同进程里前一档 free 掉的页会被 glibc 留着复用，
//! 后一档的差值读出来是 0（实测如此）。故第二张表每格起一个子进程（本测试二进制自己，
//! 由 `PHRASE_PROBE_MODE` 选档）。
//!
//! 是**探针不是判据**：数字随机器变，拿它当回归门会变成随机红。正确性由
//! `english_phrase.rs` 的对拍用例守。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use wind_dict::cached::CachedDict;
use wind_dict::{DictManager, SystemDictLayer};
use wind_engine::english_phrase::{LazyPhraseIndex, PHRASE_SEPARATOR, split_segments};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size() as isize, Relaxed) + l.size() as isize;
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Relaxed);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn live() -> isize {
    LIVE.load(Relaxed)
}

/// 从 `live` 起算的峰值增量。
fn reset_peak() -> isize {
    let l = live();
    PEAK.store(l, Relaxed);
    l
}

fn rss_anon_kb() -> Option<i64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("RssAnon:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn mb(b: isize) -> f64 {
    b as f64 / 1024.0 / 1024.0
}

const HEADS: [&str; 8] = [
    "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Omega", "Sigma",
];

fn write_synthetic_yaml(path: &std::path::Path) {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(f, "---\nname: en_probe\n...").unwrap();
    for i in 0..180_000u32 {
        let h = HEADS[(i % 8) as usize];
        let text = match i % 3 {
            0 => format!("{h}{} Beta Gamma{}", i % 5000, i % 97),
            1 => format!("{h}{} Beta", i % 5000),
            _ => format!("{h}{} Delta Epsilon Zeta{}", i % 5000, i),
        };
        let code = if i % 2 == 0 {
            text.to_ascii_lowercase().replace(' ', "")
        } else {
            text.split(' ').next().unwrap().to_ascii_lowercase()
        };
        writeln!(f, "{text}\t{code}\t{}", i % 1000).unwrap();
    }
    for i in 0..100_000u32 {
        writeln!(f, "word{i}\tword{i}\t{}", i % 500).unwrap();
    }
}

fn probe_texts(lazy: &LazyPhraseIndex, dm: &DictManager) -> Vec<Vec<String>> {
    ["alp'b'g", "be'ga", "zeta'e", "omega9'd", "sig'del'eps"]
        .iter()
        .map(|q| {
            lazy.get(dm)
                .search(&split_segments(q, PHRASE_SEPARATOR), 20)
                .into_iter()
                .map(|c| c.text)
                .collect()
        })
        .collect()
}

/// 子进程：只量一档，打一行 `ROW` 给父进程收。
fn child(mode: &str, root: &std::path::Path) {
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::load_at_with(
            &root.join("en_probe.dict.yaml"),
            &root.join("en_probe.wdat"),
            true,
        )
        .unwrap(),
        "en_probe",
    )));
    let dir = (mode != "heap").then(|| root.join("cache"));
    let rss0 = rss_anon_kb().unwrap_or(0);
    let base = live();
    let t0 = std::time::Instant::now();
    let lazy = LazyPhraseIndex::with_cache_dir(dir);
    let idx = lazy.get(&dm);
    let dt = t0.elapsed();
    // 查一遍：mmap 的页要被真的读到，量的才是「用过之后」的常驻。
    let _ = probe_texts(&lazy, &dm);
    let rss = rss_anon_kb().unwrap_or(0) - rss0;
    println!(
        "ROW {mode} {:.1}ms live={:.2}MB RssAnon={:.2}MB scans={}",
        dt.as_secs_f64() * 1000.0,
        mb(live() - base),
        rss as f64 / 1024.0,
        lazy.full_scans()
    );
    drop(idx);
}

#[test]
#[ignore = "探针，不参与常规回归"]
fn heap_vs_mmap_resident_and_timing() {
    if let (Ok(mode), Ok(root)) = (
        std::env::var("PHRASE_PROBE_MODE"),
        std::env::var("PHRASE_PROBE_ROOT"),
    ) {
        child(&mode, std::path::Path::new(&root));
        return;
    }
    let root = std::env::temp_dir().join(format!("wind_phrase_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let yaml = root.join("en_probe.dict.yaml");
    let wdat = root.join("en_probe.wdat");
    let cache = root.join("cache");
    write_synthetic_yaml(&yaml);

    let dm = Arc::new(DictManager::new());
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::load_at_with(&yaml, &wdat, true).unwrap(),
        "en_probe",
    )));

    // 基线结果（堆版），后面两档逐条对拍。
    let want = probe_texts(&LazyPhraseIndex::new(), &dm);
    assert!(want.iter().all(|v| !v.is_empty()), "探针应有命中");

    println!(
        "\n档位     轮次  耗时        常驻 live(MB)  峰值 live(MB)  RssAnon(MB)  heap_kb  mapped_kb  全表扫"
    );
    for round in 1..=3 {
        for (label, dir) in [
            ("堆版", None),
            ("首建", Some(cache.clone())),
            ("命中", Some(cache.clone())),
        ] {
            if label == "首建" {
                let _ = std::fs::remove_dir_all(&cache);
            }
            let rss0 = rss_anon_kb();
            let base = reset_peak();
            let t0 = std::time::Instant::now();
            let lazy = LazyPhraseIndex::with_cache_dir(dir);
            let idx = lazy.get(&dm);
            let dt = t0.elapsed();
            let resident = live() - base;
            let peak = PEAK.load(Relaxed) - base;
            let rss = rss_anon_kb()
                .zip(rss0)
                .map(|(a, b)| (a - b) as f64 / 1024.0);
            println!(
                "{label}     {round}     {dt:>9.1?}   {:>12.2}   {:>12.2}   {:>10}   {:>6}   {:>8}   {}",
                mb(resident),
                mb(peak),
                rss.map_or("-".into(), |r| format!("{r:.2}")),
                idx.heap_bytes() / 1024,
                idx.mapped_bytes() / 1024,
                lazy.full_scans(),
            );
            assert_eq!(probe_texts(&lazy, &dm), want, "{label} 与堆版结果不一致");
            if label == "命中" {
                assert_eq!(lazy.full_scans(), 0, "命中不该全表扫");
            }
            drop(idx);
            drop(lazy);
        }
    }
    println!("词组数 = {}", LazyPhraseIndex::new().get(&dm).len());
    drop(dm);

    // 一档一进程：RssAnon 才有意义。
    println!("\n一档一进程（RssAnon 为子进程内建表前后之差）：");
    let exe = std::env::current_exe().unwrap();
    for round in 1..=3 {
        for mode in ["heap", "build", "hit"] {
            if mode == "build" {
                let _ = std::fs::remove_dir_all(&cache);
            }
            let out = std::process::Command::new(&exe)
                .args([
                    "--ignored",
                    "--nocapture",
                    "--exact",
                    "heap_vs_mmap_resident_and_timing",
                ])
                .env("PHRASE_PROBE_MODE", mode)
                .env("PHRASE_PROBE_ROOT", &root)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            // 子进程挂了（panic、找不到词库）就在这里说清楚，别让表里静默少一行。
            assert!(
                out.status.success() && text.contains("ROW "),
                "子进程 {mode} 第 {round} 轮失败（{}）\nstdout:\n{text}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            for l in text.lines().filter(|l| l.starts_with("ROW ")) {
                println!("  轮{round} {}", &l[4..]);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
