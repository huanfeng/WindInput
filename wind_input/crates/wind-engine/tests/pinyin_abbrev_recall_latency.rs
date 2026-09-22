//! 简拼召回在**真实词库**上的单次 `convert` 耗时基准。
//!
//! 起因：`ABBREV_INDEX_LIMIT` 从 10 提到 64 后，成本是
//! `变体键数 × 取码上限 × 每码的 search_with_boundary`，而只有单键上限、没有总额。
//! 合成词库（每键塞满码、开 n_l+r_l+f_h）下实测 `lll` 从 344µs 涨到 4.99ms。
//! 真实 `cn_dicts` 里一个 3 位简拼键下到底有多少码，决定了这个倍数是否成立
//! —— 合成数据能证明上界存在，证不了现场会踩到。
//!
//! `#[ignore]`：依赖 `build_dev/data`（不随仓库分发），且是计时用例。
//! 跑法：`cargo test -p wind-engine --test pinyin_abbrev_recall_latency -- --ignored --nocapture`
//! 先例同 `tests/single_char_codes_cache.rs` 的基准用例。

use std::time::Instant;
use wind_config::Config;
use wind_engine::EngineManager;

/// 最坏形状：`l` 同属 `n↔l` 与 `r↔l` 两组，等价集 `{l,n,r}` 是唯一大于 2 的，
/// 键变体数的上界由它决定（`lll` ⇒ 3³ = 27 个变体键）。
const PROBES: &[&str] = &[
    "bcx",     // 真实场景：拜城县那条反馈
    "lqc",     // 仓内既有案例（篮球场）
    "nqc",     // 同上，模糊侧
    "lll",     // 变体数最坏（27）
    "lllnr",   // 更长的最坏形状
    "zhy",     // 双字母声母，纯声母段
    "zhge",    // 双字母声母 + 音节段
    "bcxrmzf", // 长串，逐切点走 step 6.2
];

fn data_dir() -> Option<std::path::PathBuf> {
    // 从 crate 目录上溯到仓库根。
    let mut d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..4 {
        d.pop();
        let c = d.join("build_dev/data");
        if c.join("schemas/pinyin").is_dir() {
            return Some(c);
        }
    }
    None
}

#[test]
#[ignore = "依赖 build_dev/data，且是计时用例"]
fn abbrev_recall_latency_on_real_dict() {
    run("出厂配置（模糊音全关）", false);
    // 最坏形状：`l` 的等价集 `{l,n,r}` 要靠 n_l + r_l 同时开才出现。
    run("开满声母模糊音（zh_z/ch_c/sh_s/n_l/f_h/r_l）", true);
}

fn run(label: &str, fuzzy: bool) {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };
    let mut config = Config::load(Some(&dir)).unwrap_or_default();
    if fuzzy {
        let f = &mut config.schema.pinyin.fuzzy;
        f.enabled = true;
        f.zh_z = true;
        f.ch_c = true;
        f.sh_s = true;
        f.n_l = true;
        f.f_h = true;
        f.r_l = true;
    }
    println!("\n=== {label} ===");
    let mgr = EngineManager::new(&config, Some(&dir));

    // 预热：首次会熔词库 / 建索引，不计入。
    for p in PROBES {
        let _ = mgr.convert(p, 300);
    }

    println!("\n输入      | 候选数 | 单次 convert");
    println!("----------|--------|-------------");
    for p in PROBES {
        let t = Instant::now();
        const N: u32 = 20;
        let mut n = 0;
        for _ in 0..N {
            n = mgr.convert(p, 300).candidates.len();
        }
        let per = t.elapsed() / N;
        println!("{p:9} | {n:6} | {per:?}");
        assert!(
            per.as_millis() < 50,
            "{p} 单次 convert {per:?} 超过 50ms —— 这是按键线程上的开销"
        );
    }
}
