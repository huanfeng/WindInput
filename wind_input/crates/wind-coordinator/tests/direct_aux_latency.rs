//! 辅助码的协调器级逐键耗时。**只量不判**（`#[ignore]`）；口径见
//! `docs/design/aux-code-continuous.md` §6 与附录「P3 耗时」「P3 端到端」。
//!
//! 每个（布局 × 语法模型开 / 关）四列，同一语料逐句清空后逐键敲入、每键计一次 `handle_key_event`：
//! - `old`：`direct` 关（旧路径）；
//! - `tail`：`direct` 开 + 逃生口 `continuous = false`（旧路径 + 串尾直接辅助，0.124 起开着 `direct` 的用户）；
//! - `none`：`direct` 开、辅码表为空（击键图、不给辅码）；
//! - `cont`：`direct` 开（连续辅助码）。
//!
//! 另在 `cont` 每键之后（计时区外）单独计一次生效判定 `continuous_aux_lookup`（取设置 + 建本键辅码
//! 视图），它已含在 `cont` 的每键耗时里，单列只为看占比。
//!
//! 语料同 `wind-engine/tests/aux_continuous_eval.rs` 的语料 A，各列交替各跑
//! `WIND_DIRECT_LATENCY_RUNS` 遍（默认 3）。`WIND_AUX_LAT_LAYOUTS`（默认 `xiaohe,ziranma,mspy`）、
//! `WIND_AUX_LAT_GRAMMAR`（默认 `off,on`）。
//!
//! ```text
//! cargo test -p wind-coordinator --release --test direct_aux_latency -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_engine::pinyin::dag::Dag;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::syllable::SyllableTrie;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_ESCAPE: u32 = 0x1B;
const VK_A: u32 = 0x41;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn data_dir() -> PathBuf {
    root().join("build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir().join("schemas");
    d.join("shuangpin.schema.toml").is_file()
        && d.join("aux_code/flypy_full.txt").is_file()
        && d.join("pinyin/cn_dicts/base.dict.yaml").is_file()
}

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "?".to_string())
}

fn env_list(name: &str, default: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 语料 A 在该布局下的击键（筛法同 `aux_continuous_eval::encode`）。
fn corpus(layout: &str) -> Vec<String> {
    let src = std::fs::read_to_string(
        root().join("wind_input/crates/wind-engine/tests/grammar_sentence_eval.rs"),
    )
    .expect("读语料 A");
    let conv = ShuangpinConverter::new(
        Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
            .expect("布局"),
    );
    let rev = conv.build_reverse();
    let trie = SyllableTrie::new();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(rest) = line.trim_start().strip_prefix("(\"") else {
            continue;
        };
        let Some((py, rest)) = rest.split_once("\", \"") else {
            continue;
        };
        let Some((hz, _)) = rest.split_once('"') else {
            continue;
        };
        if !py.bytes().all(|b| b.is_ascii_lowercase()) || !seen.insert(hz.to_string()) {
            continue;
        }
        let syls = Dag::build(py, &trie).maximum_match();
        if hz.chars().count() < 4 || syls.len() != hz.chars().count() {
            continue;
        }
        let syls: Vec<&str> = syls.iter().map(String::as_str).collect();
        if let Some(keys) = rev.encode_all(&syls) {
            out.push(keys);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Old,
    Tail,
    None,
    Cont,
}

const MODES: [(Mode, &str); 4] = [
    (Mode::Old, "old"),
    (Mode::Tail, "tail"),
    (Mode::None, "none"),
    (Mode::Cont, "cont"),
];

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join(format!("wind_aux_lat_{}", std::process::id()))
        .join(tag);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

fn coord(layout: &str, grammar: bool, mode: Mode, tag: &str) -> Arc<Coordinator> {
    let ov = tmp_dir(&format!("{tag}_ov"));
    let mut body = format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n");
    match mode {
        Mode::Tail => body.push_str("[engine.aux_code]\ncontinuous = false\n"),
        Mode::None => {
            let empty = ov.join("empty_aux.txt");
            std::fs::write(&empty, "# 空\n").expect("写空辅码表");
            body.push_str(&format!(
                "[engine.aux_code]\nfiles = [{:?}]\n",
                empty.display().to_string()
            ));
        }
        _ => {}
    }
    std::fs::write(ov.join("shuangpin.toml"), body).expect("写 override");
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.aux_code.enabled = true;
    cfg.schema.pinyin.aux_code.direct = mode != Mode::Old;
    if grammar {
        cfg.schema.pinyin.grammar.enabled = true;
        cfg.schema.pinyin.grammar.weight = 0.5;
        cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".to_string();
    }
    let store = tmp_dir(&format!("{tag}_store")).join("s.redb");
    Coordinator::new_headless_with_store_override(
        cfg,
        Some(&data_dir()),
        Arc::new(Store::open(&store).expect("开 store")),
        Some(ov),
    )
}

fn press(c: &Coordinator, vk: u32) {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
}

fn vk(b: u8) -> u32 {
    if b == b';' {
        wind_keys::keymap::VK_SEMICOLON
    } else {
        VK_A + u32::from(b - b'a')
    }
}

/// 一遍：逐句清空、逐键敲入，回（该键后的输入长度, 每键耗时 µs, 生效判定耗时 µs）。
fn pass(c: &Coordinator, corpus: &[String], lookup: bool) -> Vec<(usize, f64, f64)> {
    let mut out = Vec::new();
    for s in corpus {
        press(c, VK_ESCAPE);
        for (i, b) in s.bytes().enumerate() {
            let t = Instant::now();
            press(c, vk(b));
            let key = t.elapsed().as_secs_f64() * 1e6;
            let lk = if lookup {
                let t = Instant::now();
                std::hint::black_box(c.debug_continuous_aux_lookup());
                t.elapsed().as_secs_f64() * 1e6
            } else {
                0.0
            };
            out.push((i + 1, key, lk));
        }
    }
    press(c, VK_ESCAPE);
    out
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

type Run = Vec<(usize, f64, f64)>;

/// 切法：名字 + 按「该键后的输入长度」取舍。
type Cut = (&'static str, fn(usize) -> bool);

/// (各遍均值的平均, 各遍 P95 的平均, 逐键各遍最小后取最大)
fn stats(runs: &[Run], keep: fn(usize) -> bool, pick: fn(&(usize, f64, f64)) -> f64) -> [f64; 3] {
    let idx: Vec<usize> = (0..runs[0].len()).filter(|&i| keep(runs[0][i].0)).collect();
    let (mut mean, mut p95) = (0.0, 0.0);
    for r in runs {
        let mut v: Vec<f64> = idx.iter().map(|&i| pick(&r[i])).collect();
        mean += v.iter().sum::<f64>() / v.len() as f64;
        v.sort_by(|a, b| a.total_cmp(b));
        p95 += pct(&v, 0.95);
    }
    let best_max = idx
        .iter()
        .map(|&i| {
            runs.iter()
                .map(|r| pick(&r[i]))
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0.0, f64::max);
    let n = runs.len() as f64;
    [mean / n, p95 / n, best_max]
}

fn even(n: usize) -> bool {
    n.is_multiple_of(2)
}

fn odd(n: usize) -> bool {
    !n.is_multiple_of(2)
}

#[test]
#[ignore = "耗时测量：慢，依赖 build_dev 真实词库与辅码表"]
fn direct_aux_corpus_latency() {
    if !has_data() {
        eprintln!("跳过：build_dev/data 缺双拼方案 / 辅码表 / 词库");
        return;
    }
    let runs: usize = std::env::var("WIND_DIRECT_LATENCY_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    for layout in env_list("WIND_AUX_LAT_LAYOUTS", "xiaohe,ziranma,mspy") {
        let corpus = corpus(&layout);
        assert!(corpus.len() > 100, "语料只有 {} 句", corpus.len());
        for g in env_list("WIND_AUX_LAT_GRAMMAR", "off,on") {
            let grammar = g == "on";
            let tag = format!("a_{layout}_gram{g}");
            let cs: Vec<Arc<Coordinator>> = MODES
                .iter()
                .map(|(m, name)| coord(&layout, grammar, *m, &format!("{tag}_{name}")))
                .collect();
            for (c, (m, _)) in cs.iter().zip(MODES) {
                let _ = pass(c, &corpus, m == Mode::Cont);
            }
            let load0 = loadavg();
            let mut res: Vec<Vec<Run>> = vec![Vec::new(); MODES.len()];
            for _ in 0..runs {
                for (i, (c, (m, _))) in cs.iter().zip(MODES).enumerate() {
                    res[i].push(pass(c, &corpus, m == Mode::Cont));
                }
            }
            eprintln!(
                "# [{tag}] runs={runs} keys={} loadavg before={load0} after={}",
                res[0][0].len(),
                loadavg()
            );
            let cuts: [Cut; 3] = [("all", |_| true), ("even", even), ("odd", odd)];
            for (name, keep) in cuts {
                let mut line = format!("# [{tag}] {name}:");
                for (i, (_, mname)) in MODES.iter().enumerate() {
                    let [m, p, b] = stats(&res[i], keep, |x| x.1);
                    line.push_str(&format!(
                        " {mname} mean={m:.1}us p95={p:.1}us bestmax={b:.1}us |"
                    ));
                }
                let [m, p, b] = stats(&res[3], keep, |x| x.2);
                line.push_str(&format!(
                    " lookup mean={m:.2}us p95={p:.2}us bestmax={b:.2}us"
                ));
                eprintln!("{line}");
            }
        }
    }
}
