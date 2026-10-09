//! 探针：切到**未加载**方案时 `ensure_loaded` 的耗时，缓存新鲜 / 过期两种情形
//! （方案常驻策略出厂关的前提，设计 `docs/design/memory-footprint.md` §9 D1）。
//!
//! 依赖 `build_dev/data` 真实词库，故 `#[ignore]`；须 release：
//!
//! ```text
//! cargo test --release -p wind-engine --test schema_switch_latency_probe -- --ignored --nocapture
//! ```
//!
//! 缓存根经 `XDG_DATA_HOME` 重定向到临时目录（本二进制只有这一条用例，进程级的缓存根
//! `OnceLock` 在这里第一次初始化）：「过期」= 整个缓存根删掉，不碰开发机真实缓存。
//! 两次之间用 `evict_idle(0)` 摘掉目标（活跃方案是小方案 stroke，不在测量范围内），再等 2 秒
//! 让后台线程放手词库映射。
//! 「新鲜」那几次的页缓存是热的——与生产一致：启动后台刚校验过缓存。

use std::path::PathBuf;
use std::time::{Duration, Instant};
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn evict_all(em: &EngineManager) {
    while !em.evict_idle(Duration::ZERO).is_empty() {}
}

#[test]
#[ignore]
fn switch_to_unloaded_schema_fresh_vs_stale_cache() {
    let data = data_dir();
    if !data.join("schemas/pinyin.schema.toml").exists() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let xdg = std::env::temp_dir().join(format!("wind_switch_probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg);
    std::fs::create_dir_all(&xdg).unwrap();
    // SAFETY: 本二进制只有这一条用例，此时尚无任何线程读环境变量。
    unsafe {
        std::env::set_var("XDG_DATA_HOME", &xdg);
    }
    let cache = Config::cache_dir().expect("缓存根");
    assert!(
        cache.starts_with(&xdg),
        "缓存根须已重定向：{}",
        cache.display()
    );

    let mut cfg = Config::default();
    cfg.schema.active = "stroke".into();
    cfg.schema.available = vec![
        "stroke".into(),
        "wubi86".into(),
        "pinyin".into(),
        "wubi86_pinyin".into(),
    ];
    cfg.input.temp_pinyin.enabled = false;
    cfg.input.temp_english.show_candidates = false;
    let em = EngineManager::new(&cfg, Some(&data));

    println!("\n方案            情形   第1次      第2次      第3次");
    for id in ["wubi86", "pinyin", "wubi86_pinyin"] {
        for stale in [true, false] {
            let mut ms = Vec::new();
            for _ in 0..3 {
                evict_all(&em);
                // 等上一次加载顺带起的后台线程（码表整句预热、英文词组索引）放手：它们握着
                // 词库的 `Arc`，wdat 读者池按路径弱引用共享，不等的话「新鲜」量到的是复用映射。
                std::thread::sleep(Duration::from_secs(2));
                if stale {
                    let _ = std::fs::remove_dir_all(&cache);
                    std::fs::create_dir_all(&cache).unwrap();
                }
                let t0 = Instant::now();
                assert!(em.prewarm_schema(id), "{id} 应能加载");
                ms.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "{id:<15} {}   {:>8.1}ms {:>8.1}ms {:>8.1}ms",
                if stale { "过期" } else { "新鲜" },
                ms[0],
                ms[1],
                ms[2]
            );
        }
    }
    let _ = std::fs::remove_dir_all(&xdg);
}
