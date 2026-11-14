//! 「零权重读音让位」的扫参探针 —— **测量工具，不是门禁**（门禁在
//! `wind-coordinator/tests/pinyin_zero_weight_yield.rs`）。
//!
//! 扫两张表，逐条打印**界面序**（与协调器同一个比较器、同文去重，见 `pinyin_eval.rs`
//! 的「两套位次」）首选与前 3：
//! - 双拼：全部内置布局 × 全部两键组合（`a..z` + `;`）；
//! - 全拼：标准音节表的每个音节。
//!
//! 改前改后各跑一次、`diff` 两份输出即得变化清单：
//!
//! ```text
//! WIND_ZW_OUT=/path/before.tsv \
//!   cargo test -p wind-engine --test pinyin_zero_weight_yield -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;
use wind_engine::pinyin::syllable::STANDARD_SYLLABLES;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn schema_config(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg
}

/// 布局经 `schema_overrides/shuangpin.toml` 切换（与设置页写入的是同一个载体）。
fn shuangpin_manager(dir: &Path, layout: &str, tmp: &Path) -> EngineManager {
    let ov = tmp.join(format!("ov-{layout}"));
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("shuangpin.toml"),
        format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"),
    )
    .unwrap();
    EngineManager::with_store_override(&schema_config("shuangpin"), Some(dir), None, Some(ov))
}

fn ui_texts(mgr: &EngineManager, schema: &str, input: &str) -> Vec<String> {
    let mut c = mgr.convert_with(schema, input, 300).candidates;
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    let mut seen = std::collections::HashSet::new();
    c.retain(|x| seen.insert(x.text.clone()));
    c.into_iter().map(|c| c.text).collect()
}

/// 每行打前 3；设 `WIND_ZW_FULL=1` 时打全表（看首屏以外的位次变化）。
fn row(out: &mut String, tag: &str, input: &str, texts: &[String]) {
    let n = if std::env::var_os("WIND_ZW_FULL").is_some() {
        usize::MAX
    } else {
        3
    };
    let top3: Vec<&str> = texts.iter().take(n).map(String::as_str).collect();
    let _ = writeln!(
        out,
        "{tag}\t{input}\t{}\t{}",
        texts.first().map(String::as_str).unwrap_or("-"),
        top3.join(" ")
    );
}

#[test]
#[ignore = "扫参探针：依赖 build_dev 真实词库，用 --ignored 显式运行"]
fn sweep_two_key_and_single_syllable_top1() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("wind-zw-sweep-{}", std::process::id()));
    let mut out = String::new();

    let mut keys: Vec<char> = ('a'..='z').collect();
    keys.push(';');
    for layout in [
        "xiaohe", "sogou", "mspy", "ziranma", "abc", "ziguang", "shoudao", "jiajia", "gb2017",
    ] {
        let mgr = shuangpin_manager(&dir, layout, &tmp);
        assert_eq!(mgr.shuangpin_layout_of("shuangpin"), layout);
        for &a in &keys {
            for &b in &keys {
                let input: String = [a, b].iter().collect();
                row(
                    &mut out,
                    layout,
                    &input,
                    &ui_texts(&mgr, "shuangpin", &input),
                );
            }
        }
    }

    let mgr = EngineManager::with_store_override(&schema_config("pinyin"), Some(&dir), None, None);
    for syl in STANDARD_SYLLABLES {
        row(&mut out, "quanpin", syl, &ui_texts(&mgr, "pinyin", syl));
    }

    let _ = std::fs::remove_dir_all(&tmp);
    match std::env::var("WIND_ZW_OUT") {
        Ok(p) => std::fs::write(&p, &out).unwrap(),
        Err(_) => print!("{out}"),
    }
}
