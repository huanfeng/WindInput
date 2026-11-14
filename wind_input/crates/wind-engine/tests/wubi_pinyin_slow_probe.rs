//! 临时探针：容器上 Dev 实例的 wubi86_pinyin（mixed）方案 + 拼音 Dev 配置计时同批键。
//! 运行：cargo test -p wind-engine --test wubi_pinyin_slow_probe -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use wind_config::Config;
use wind_engine::EngineManager;

#[test]
#[ignore]
fn convert_latencies() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    let mut cfg = Config::default();
    cfg.schema.available = vec![
        "wubi86".to_string(),
        "wubi86_pinyin".to_string(),
        "pinyin".to_string(),
    ];
    cfg.schema.active = "wubi86_pinyin".to_string();
    // Dev 实例拼音侧生效的差异
    let f = &mut cfg.schema.pinyin.fuzzy;
    f.enabled = true;
    f.en_eng = true;
    f.in_ing = true;
    cfg.schema.pinyin.english_merge.enable = true;
    cfg.schema.pinyin.grammar.enabled = true;
    cfg.schema.pinyin.grammar.model = "wanxiang-lts-zh-hans.gram".into();
    cfg.schema.mix.enable_english = true;
    cfg.schema.mix.enable_pinyin_abbrev = true;
    let mgr = EngineManager::new(&cfg, Some(&dir));
    for c in [
        "ga", "zh", "fj", "fjp", "gmd", "khy", "yy", "dmh", "talk", "nihao", "ap",
    ] {
        let mut times = Vec::new();
        let mut n_last = 0usize;
        for _ in 0..5 {
            let t = Instant::now();
            let r = mgr.convert(c, 50);
            times.push(t.elapsed().as_micros());
            n_last = r.candidates.len();
        }
        times.sort();
        println!(
            "{c:>6}  med={:>6}µs  min={:>6}µs  n={}",
            times[2], times[0], n_last
        );
    }
}
