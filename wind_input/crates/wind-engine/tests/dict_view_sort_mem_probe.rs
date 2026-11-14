//! 系统词库整库排序（`sysDict.entries` 带 `sort`）的**堆内存**探针（`#[ignore]`，手动跑）。
//!
//! ```text
//! cargo test -p wind-engine --release --test dict_view_sort_mem_probe -- --ignored --nocapture
//! ```
//!
//! 拼音主库（build_dev/data，约 65 万条）按权重 / 编码 / 词条各排一次，用计数分配器记：
//! 排序那一次调用的**峰值**堆增量（整读文件 + 逐行解析 + 排序键 + 结果），以及调用返回后
//! **留下**的（命中缓存）。先数一遍条数，把检查点索引建好，免得记进第一列。
//!
//! 是**探针不是判据**：数字随词库变。正确性由 `dict_view.rs` 的用例守。

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use wind_config::Config;
use wind_engine::EngineManager;
use wind_engine::manager::{EntryQuery, EntrySort};

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

fn mb(b: isize) -> f64 {
    b as f64 / 1024.0 / 1024.0
}

#[test]
#[ignore]
fn sort_memory() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    assert!(
        data.join("schemas/pinyin.schema.toml").exists(),
        "缺 build_dev/data"
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    let m = EngineManager::new(&cfg, Some(&data));
    let rel = "pinyin/rime_frost.dict.yaml";
    let ty = "rime_pinyin";
    let n = m.dict_file_entries(rel, ty).unwrap();
    for (sort, name) in [
        (EntrySort::Weight, "权重"),
        (EntrySort::Code, "编码"),
        (EntrySort::Text, "词条"),
    ] {
        let q = EntryQuery {
            sort: Some(sort),
            desc: true,
            ..Default::default()
        };
        let base = LIVE.load(Relaxed);
        PEAK.store(base, Relaxed);
        let t = std::time::Instant::now();
        let p = m.dict_entries(rel, ty, &q, 0, 100).unwrap();
        let took = t.elapsed();
        drop(p);
        eprintln!(
            "按{name}排序 {n} 条：{took:?}，峰值 +{:.1} MB，留下 +{:.1} MB",
            mb(PEAK.load(Relaxed) - base),
            mb(LIVE.load(Relaxed) - base)
        );
    }
}
