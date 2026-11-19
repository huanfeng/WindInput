//! 双拼击键坐标图（`ConvertOptions::sp_raw_lattice`）新旧差异导出。**不是门禁**，是审视工具。
//!
//! 语料取 `grammar_sentence_eval` 的整句（去重、≥4 字、最大匹配音节数 = 字数），经
//! `ShuangpinReverse::encode_all` 转成小鹤击键；对每句的每个逐键前缀与手挑边界输入，
//! 同一进程里分别以开关关 / 开调用引擎，只导出有差异的行。
//!
//! ```text
//! cargo test -p wind-engine --release --test shuangpin_raw_parity -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 输出：`spike-results/aux-continuous/p1_diff.tsv`（出厂 1 条整句）、`p1_diff_nbest.tsv`
//! （整句露 3 算 5，另比整句池）、`p1_hand.tsv`（手挑输入的开 / 关前 5 条，不论有无差异）。
//!
//! 位次取界面序：与 `pinyin_eval::ui_candidates` 同一串加工（消费长度为首键重排）。
//!
//! 设 `WIND_RAW_PARITY_DUMP=<目录>` 时另把每条输入开 / 关两侧的引擎原始候选与整句池（`Debug`
//! 全字段）写到 `<目录>/{default,nbest}.{off,on}.txt`，供纯性能改动逐字节对拍。
//!
//! `raw_aux_timing`（同样 `--ignored`，加 `--test shuangpin_raw_parity raw_aux_timing` 单跑）：开关开时
//! 辅码表 `None` 与小鹤辅码表的逐键耗时对比，只打到 stderr、不写文件。

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use wind_candidate::Candidate;
use wind_config::Config;
use wind_engine::pinyin::dag::Dag;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter, ShuangpinReverse};
use wind_engine::pinyin::syllable::SyllableTrie;
use wind_engine::{ConvertOptions, EngineManager};

/// 与协调器传给引擎的上限同值。
const LIMIT: usize = 300;
/// 比较的界面序条数。
const CMP_N: usize = 5;
const TIMING_RUNS: usize = 3;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn data_dir() -> Option<PathBuf> {
    let p = root().join("build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 出厂 `shuangpin.schema.toml` 即小鹤。`nbest` 时整句露 3 条、算 5 条。
fn manager(dir: &Path, nbest: bool) -> EngineManager {
    schema_manager(dir, "shuangpin", nbest)
}

fn schema_manager(dir: &Path, schema: &str, nbest: bool) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.to_string()];
    cfg.schema.active = schema.to_string();
    if nbest {
        cfg.schema.pinyin.sentence_count = 3;
        cfg.schema.pinyin.sentence_max_count = 5;
    }
    EngineManager::new(&cfg, Some(dir))
}

fn xiaohe() -> ShuangpinConverter {
    let path = root().join("data/schemas/shuangpin/xiaohe.toml");
    ShuangpinConverter::new(Layout::from_toml(&path).expect("加载小鹤布局"))
}

/// `grammar_sentence_eval.rs` 里的 `("拼音", "汉字")` 行。
fn sentences() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/grammar_sentence_eval.rs"),
    )
    .expect("读语料");
    let mut v = Vec::new();
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
        if py.bytes().all(|b| b.is_ascii_lowercase()) {
            v.push((py.to_string(), hz.to_string()));
        }
    }
    v
}

/// (击键, 汉字)。筛法同 spike：去重、≥4 字、最大匹配音节数 = 字数、小鹤可编码。
fn corpus(rev: &ShuangpinReverse) -> Vec<(String, String)> {
    let trie = SyllableTrie::new();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (py, hz) in sentences() {
        if !seen.insert(hz.clone()) {
            continue;
        }
        let n = hz.chars().count();
        let syls = Dag::build(&py, &trie).maximum_match();
        if n < 4 || syls.len() != n {
            continue;
        }
        let syls: Vec<&str> = syls.iter().map(String::as_str).collect();
        if let Some(keys) = rev.encode_all(&syls) {
            out.push((keys, hz));
        }
    }
    out
}

/// 手挑：键对拼不出、无残码、`'` 各位置、奇偶长、残码整句。
const HAND: &[&str] = &[
    "pl",
    "nipl",
    "niplhc",
    "niplhcm",
    "nihcpl",
    "nihcplm",
    "nihcmd",
    "vsplgo",
    "plni",
    "wopl",
    "nipln",
    "ni'hc",
    "ni'hcm",
    "ni'hc'm",
    "'nihc",
    "nihc'",
    "nihc'm",
    "n'hc",
    "nihcm",
    "buvidck",
    "fjgjk",
    "piajb",
    "qkwfxmz",
    "nizotmqun",
    "vshxrfmbg",
    "wouivsgor",
    "yrbek",
    "zhuuuiuu",
    "wom",
    "a",
    "ab",
];

fn opts(raw: bool) -> ConvertOptions {
    ConvertOptions {
        sp_raw_lattice: raw,
        ..Default::default()
    }
}

/// 界面序（同 `pinyin_eval::ui_candidates`）。
fn ui_order(mut c: Vec<Candidate>, input: &str) -> Vec<Candidate> {
    wind_candidate::drop_corrected_duplicates(&mut c);
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    let mut seen = HashSet::new();
    c.retain(|x| seen.insert(x.text.clone()));
    wind_candidate::protect_top_from_corrected(&mut c, input.len());
    c
}

struct Out {
    /// 引擎原始候选与整句池的 `Debug` 全文（对拍用）。
    raw: String,
    ui: Vec<Candidate>,
    engine_top1: String,
    pool: Vec<Candidate>,
}

fn run(mgr: &EngineManager, keys: &str, raw: bool) -> Out {
    let r = mgr.convert_with_opts("shuangpin", keys, LIMIT, opts(raw));
    Out {
        raw: format!("{keys}\t{:?}\t{:?}\n", r.candidates, r.sentence_pool),
        engine_top1: r
            .candidates
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default(),
        pool: r.sentence_pool,
        ui: ui_order(r.candidates, keys),
    }
}

fn pool_of(o: &Out) -> Vec<(&str, usize)> {
    o.pool
        .iter()
        .map(|c| (c.text.as_str(), c.consumed_length))
        .collect()
}

fn fmt_list(v: &[(&str, usize)]) -> String {
    v.iter()
        .map(|(s, n)| format!("{s}/{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 参照：同一串转成全拼（含残码声母）后全拼方案的界面首选。
fn fullpinyin_top1(py: &EngineManager, conv: &ShuangpinConverter, keys: &str) -> String {
    let q = conv.convert(keys).full_pinyin.replace('\'', "");
    let r = py.convert_with_opts("pinyin", &q, LIMIT, ConvertOptions::default());
    let ui = ui_order(r.candidates, &q);
    ui.first()
        .map(|c| format!("{}/{}", c.text, c.consumed_length))
        .unwrap_or_default()
}

fn head(o: &Out) -> Vec<(&str, usize)> {
    o.ui.iter()
        .take(CMP_N)
        .map(|c| (c.text.as_str(), c.consumed_length))
        .collect()
}

fn join3(o: &Out) -> String {
    o.ui.iter()
        .take(3)
        .map(|c| format!("{}/{}", c.text, c.consumed_length))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 击键图整句的消费数短于旧口径（按码回映射）的跨度：(文字, 实际, 回映射)。
fn short_consumed(conv: &ShuangpinConverter, keys: &str, o: &Out) -> Vec<(String, usize, usize)> {
    let r = conv.convert(keys);
    let query = r.full_pinyin.replace('\'', "");
    o.ui.iter()
        .chain(o.pool.iter())
        .filter(|c| c.is_sentence && c.consumed_length != 0)
        .filter_map(|c| {
            let fp = if !c.code.is_empty() && query.starts_with(&c.code) {
                c.code.len()
            } else {
                query.len()
            };
            let implied = r.map_consumed_length(fp);
            (c.consumed_length < implied).then(|| (c.text.clone(), c.consumed_length, implied))
        })
        .collect()
}

/// 机制提示（人工归类前的粗分）。
fn hint(keys: &str, target: Option<&str>, old: &Out, new: &Out) -> String {
    let (ot, nt) = (old.ui.first(), new.ui.first());
    let (otx, ntx) = (
        ot.map_or("", |c| c.text.as_str()),
        nt.map_or("", |c| c.text.as_str()),
    );
    let mut tags = Vec::new();
    if let Some(t) = target {
        match (otx == t, ntx == t) {
            (false, true) => tags.push("full:new_hits"),
            (true, false) => tags.push("full:new_misses"),
            (false, false) if otx != ntx => tags.push("full:both_miss"),
            _ => {}
        }
    }
    let old_texts: HashSet<&str> = old.ui.iter().take(CMP_N).map(|c| c.text.as_str()).collect();
    let new_texts: HashSet<&str> = new.ui.iter().take(CMP_N).map(|c| c.text.as_str()).collect();
    let extra_full = new.ui.iter().take(CMP_N).any(|c| {
        c.is_sentence && c.consumed_length == keys.len() && !old_texts.contains(c.text.as_str())
    });
    if extra_full {
        tags.push("new_full_sentence");
    }
    if old_texts == new_texts {
        tags.push(
            if otx == ntx && ot.map(|c| c.consumed_length) != nt.map(|c| c.consumed_length) {
                "consumed_only"
            } else {
                "same_set"
            },
        );
    } else if !extra_full {
        tags.push("set_changed");
    }
    tags.join(",")
}

#[derive(Default)]
struct Tally {
    total: usize,
    differ: usize,
    top1_text: usize,
    top1_consumed: usize,
    engine_top1: usize,
    /// 整句池文字不同。
    pool_differ: usize,
    /// 整句池文字相同、只有消费数不同。
    pool_consumed_only: usize,
    /// 其中新路径池内有条目消费数 < 击键数（旧路径的 0 = 整串，选它会吞掉残码）。
    pool_short_of_buffer: usize,
    short: usize,
    /// 语料里奇数长（末键落单）的逐键前缀数。
    odd: usize,
    /// [关, 开]：其中界面首选是消费整串的整句（step 2c 残码整句）的条数。
    odd_full: [usize; 2],
    /// [关, 开]：上一项里首选恰是来源句同长前缀（残码猜的字也对）的条数。
    odd_full_hit: [usize; 2],
}

/// 奇数长前缀上首选为残码整句时计数，并看它是否猜中来源句。
fn tally_odd(t: &mut Tally, inp: &Input, outs: [&Out; 2]) {
    let Some(src) = inp.src.as_deref().filter(|_| inp.keys.len() % 2 == 1) else {
        return;
    };
    t.odd += 1;
    for (i, o) in outs.into_iter().enumerate() {
        let Some(top) = o.ui.first() else { continue };
        if top.is_sentence && top.consumed_length == inp.keys.len() {
            t.odd_full[i] += 1;
            t.odd_full_hit[i] += usize::from(src.starts_with(top.text.as_str()));
        }
    }
}

struct Input {
    keys: String,
    /// 来源句（手挑输入为空）。
    src: Option<String>,
    /// 前缀即整句。
    full: bool,
}

fn compare_all(
    mgr: &EngineManager,
    py: &EngineManager,
    conv: &ShuangpinConverter,
    inputs: &[Input],
    nbest: bool,
) -> (Tally, String, Vec<String>, [String; 2]) {
    let mut t = Tally::default();
    let mut rows = String::new();
    let mut shorts = Vec::new();
    let mut dump = [String::new(), String::new()];
    for inp in inputs {
        let keys = &inp.keys;
        let target = inp.full.then(|| inp.src.clone()).flatten();
        t.total += 1;
        let old = run(mgr, keys, false);
        let new = run(mgr, keys, true);
        dump[0].push_str(&old.raw);
        dump[1].push_str(&new.raw);
        for (text, got, implied) in short_consumed(conv, keys, &new) {
            t.short += 1;
            shorts.push(format!("{keys}\t{text}\t{got}\t{implied}"));
        }
        let texts = |o: &Out| o.pool.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        let pool_diff = nbest && texts(&old) != texts(&new);
        t.pool_differ += usize::from(pool_diff);
        if nbest && !pool_diff && pool_of(&old) != pool_of(&new) {
            t.pool_consumed_only += 1;
            t.pool_short_of_buffer += usize::from(
                new.pool
                    .iter()
                    .any(|c| c.consumed_length != 0 && c.consumed_length < keys.len()),
            );
        }
        tally_odd(&mut t, inp, [&old, &new]);
        let engine_diff = old.engine_top1 != new.engine_top1;
        t.engine_top1 += usize::from(engine_diff);
        if head(&old) == head(&new) && !pool_diff && !engine_diff {
            continue;
        }
        t.differ += 1;
        let (ot, nt) = (old.ui.first(), new.ui.first());
        let top1_text = ot.map(|c| &c.text) != nt.map(|c| &c.text);
        let top1_consumed = ot.map(|c| c.consumed_length) != nt.map(|c| c.consumed_length);
        t.top1_text += usize::from(top1_text);
        t.top1_consumed += usize::from(top1_consumed);
        let mut h = hint(keys, target.as_deref(), &old, &new);
        if engine_diff {
            h.push_str(",engine_top1");
        }
        if pool_diff {
            h.push_str(",pool");
        }
        let _ = writeln!(
            rows,
            "{keys}\t{}{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{h}",
            if inp.full { "=" } else { "~" },
            inp.src.as_deref().unwrap_or(""),
            ot.map_or("", |c| c.text.as_str()),
            nt.map_or("", |c| c.text.as_str()),
            join3(&old),
            join3(&new),
            ot.map_or(0, |c| c.consumed_length),
            nt.map_or(0, |c| c.consumed_length),
            old.engine_top1,
            new.engine_top1,
            fullpinyin_top1(py, conv, keys),
            fmt_list(&pool_of(&old)),
            fmt_list(&pool_of(&new)),
        );
    }
    (t, rows, shorts, dump)
}

fn write_dump(tag: &str, dump: &[String; 2]) {
    let Some(dir) = std::env::var_os("WIND_RAW_PARITY_DUMP") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("建对拍目录");
    for (side, text) in ["off", "on"].iter().zip(dump) {
        std::fs::write(dir.join(format!("{tag}.{side}.txt")), text).expect("写对拍文件");
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

/// 一遍计时的 (均值, P95, 最大值)，单位 µs。
type Pass = (f64, f64, f64);

/// 逐前缀各调一次（模拟逐键），另回逐键耗时（与 `keys` 同序）。
fn time_pass(
    mgr: &EngineManager,
    keys: &[String],
    o: impl Fn() -> ConvertOptions,
) -> (Pass, Vec<f64>) {
    let per_key: Vec<f64> = keys
        .iter()
        .map(|k| {
            let t = Instant::now();
            let r = mgr.convert_with_opts("shuangpin", k, LIMIT, o());
            let us = t.elapsed().as_secs_f64() * 1e6;
            std::hint::black_box(r);
            us
        })
        .collect();
    let mut v = per_key.clone();
    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (
        (mean, pct(&v, 0.95), v.last().copied().unwrap_or(0.0)),
        per_key,
    )
}

/// 逐键取各遍最小值后的最大值：滤掉调度抖动造成的单次尖峰，看最慢那条输入本身的耗时。
fn max_of_best(runs: &[Vec<f64>]) -> f64 {
    (0..runs[0].len())
        .map(|i| runs.iter().map(|r| r[i]).fold(f64::INFINITY, f64::min))
        .fold(0.0, f64::max)
}

/// `src` 前缀 `=` 表示该行就是整句，`~` 表示是该句的逐键前缀。
const HEADER: &str = "keys\tsrc\told_top1\tnew_top1\told_top3\tnew_top3\told_consumed\tnew_consumed\told_engine_top1\tnew_engine_top1\tfullpinyin_top1\told_pool\tnew_pool\thint\n";

fn summary(name: &str, t: &Tally) -> String {
    format!(
        "# {name}: prefixes={} differ={} top1_text_differs={} top1_consumed_differs={} engine_top1_differs={} pool_text_differs={} pool_consumed_only={} (new pool consumed < buffer: {}) short_consumed={}\n# {name}: odd-length corpus prefixes={} | top1 is residual-code full sentence: OFF {} (src-prefix hit {}) ON {} (src-prefix hit {})\n",
        t.total,
        t.differ,
        t.top1_text,
        t.top1_consumed,
        t.engine_top1,
        t.pool_differ,
        t.pool_consumed_only,
        t.pool_short_of_buffer,
        t.short,
        t.odd,
        t.odd_full[0],
        t.odd_full_hit[0],
        t.odd_full[1],
        t.odd_full_hit[1]
    )
}

#[test]
#[ignore = "差异导出：慢，依赖 build_dev 真实词库"]
fn raw_vs_old_diff() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let conv = xiaohe();
    let rev = conv.build_reverse();
    let corpus = corpus(&rev);
    eprintln!("语料 {} 句", corpus.len());
    // 语料没解析出来时别往下跑，免得覆盖已入库的差异表。
    assert!(corpus.len() > 100, "语料只有 {} 句", corpus.len());

    // 逐键前缀（跨句去重），带上来源句。
    let mut seen = HashSet::new();
    let mut inputs: Vec<Input> = Vec::new();
    for (keys, hz) in &corpus {
        for n in 1..=keys.len() {
            let p = keys[..n].to_string();
            if seen.insert(p.clone()) {
                inputs.push(Input {
                    keys: p,
                    src: Some(hz.clone()),
                    full: n == keys.len(),
                });
            }
        }
    }
    let prefix_count = inputs.len();
    for h in HAND {
        if seen.insert(h.to_string()) {
            inputs.push(Input {
                keys: h.to_string(),
                src: None,
                full: false,
            });
        }
    }

    let out_dir = root().join("spike-results/aux-continuous");
    std::fs::create_dir_all(&out_dir).expect("建输出目录");

    let mgr = manager(&dir, false);
    let py = schema_manager(&dir, "pinyin", false);
    let (tally, rows, shorts, dump) = compare_all(&mgr, &py, &conv, &inputs, false);
    write_dump("default", &dump);

    // 耗时：只计语料前缀；先预热一遍，再开 / 关交替各跑 TIMING_RUNS 遍。
    let keys: Vec<String> = inputs[..prefix_count]
        .iter()
        .map(|i| i.keys.clone())
        .collect();
    let mut timing = String::new();
    let avg = |v: &[Pass], f: fn(&Pass) -> f64| v.iter().map(f).sum::<f64>() / v.len() as f64;
    let mut bench = |label: &str, keys: &[String], partial_final: Option<bool>| {
        let o = |raw: bool| {
            move || ConvertOptions {
                sp_raw_lattice: raw,
                allow_partial_final: partial_final,
                ..Default::default()
            }
        };
        let _ = time_pass(&mgr, keys, o(false));
        let _ = time_pass(&mgr, keys, o(true));
        let (mut offs, mut ons) = (Vec::new(), Vec::new());
        let (mut off_keys, mut on_keys) = (Vec::new(), Vec::new());
        for i in 0..TIMING_RUNS {
            let (off, off_k) = time_pass(&mgr, keys, o(false));
            let (on, on_k) = time_pass(&mgr, keys, o(true));
            offs.push(off);
            ons.push(on);
            off_keys.push(off_k);
            on_keys.push(on_k);
            let _ = writeln!(
                timing,
                "# timing [{label}] run {}: OFF mean={:.1}us p95={:.1}us max={:.1}us | ON mean={:.1}us p95={:.1}us max={:.1}us | mean Δ={:+.2}% ({:+.1}us) p95 Δ={:+.1}us max Δ={:+.1}us",
                i + 1,
                off.0,
                off.1,
                off.2,
                on.0,
                on.1,
                on.2,
                (on.0 / off.0 - 1.0) * 100.0,
                on.0 - off.0,
                on.1 - off.1,
                on.2 - off.2
            );
        }
        let (m0, m1) = (avg(&offs, |p| p.0), avg(&ons, |p| p.0));
        let (p0, p1) = (avg(&offs, |p| p.1), avg(&ons, |p| p.1));
        let (x0, x1) = (avg(&offs, |p| p.2), avg(&ons, |p| p.2));
        let (b0, b1) = (max_of_best(&off_keys), max_of_best(&on_keys));
        let _ = writeln!(
            timing,
            "# timing [{label}] avg of {TIMING_RUNS}: OFF mean={m0:.1}us p95={p0:.1}us max={x0:.1}us | ON mean={m1:.1}us p95={p1:.1}us max={x1:.1}us | mean Δ={:+.2}% ({:+.1}us) p95 Δ={:+.1}us max Δ={:+.1}us | per-key best-of-{TIMING_RUNS} max: OFF={b0:.1}us ON={b1:.1}us Δ={:+.1}us (keys per run={})",
            (m1 / m0 - 1.0) * 100.0,
            m1 - m0,
            p1 - p0,
            x1 - x0,
            b1 - b0,
            keys.len()
        );
    };
    bench("all prefixes", &keys, None);
    // 拆分：偶数长（只有 step 2）/ 奇数长（step 2 + 2c）/ 关掉 2c。
    let even: Vec<String> = keys.iter().filter(|k| k.len() % 2 == 0).cloned().collect();
    let odd: Vec<String> = keys.iter().filter(|k| k.len() % 2 == 1).cloned().collect();
    bench("even length", &even, None);
    bench("odd length", &odd, None);
    bench("all, step 2c off", &keys, Some(false));

    let mut body = format!(
        "# corpus sentences={} unique_prefixes={} hand={}\n",
        corpus.len(),
        prefix_count,
        inputs.len() - prefix_count
    );
    body.push_str(&summary("default", &tally));
    body.push_str(&timing);
    body.push_str(HEADER);
    body.push_str(&rows);
    std::fs::write(out_dir.join("p1_diff.tsv"), &body).expect("写 p1_diff.tsv");
    eprint!("{}", body.lines().take(20).collect::<Vec<_>>().join("\n"));
    eprintln!();
    eprintln!("short_consumed（击键图整句消费数短于按码回映射的跨度）:");
    for s in &shorts {
        eprintln!("  {s}");
    }

    // 手挑输入：开 / 关前 5 条与整句池全量列出。
    let mut hand = String::from("keys\told_top5\tnew_top5\told_pool\tnew_pool\n");
    let mgr_n = manager(&dir, true);
    for h in HAND {
        for (tag, m) in [("default", &mgr), ("nbest", &mgr_n)] {
            let (old, new) = (run(m, h, false), run(m, h, true));
            let _ = writeln!(
                hand,
                "{h} [{tag}]\t{}\t{}\t{}\t{}",
                fmt_list(&head(&old)),
                fmt_list(&head(&new)),
                fmt_list(&pool_of(&old)),
                fmt_list(&pool_of(&new))
            );
        }
    }
    std::fs::write(out_dir.join("p1_hand.tsv"), &hand).expect("写 p1_hand.tsv");

    // 整句 N-best：另比整句池。池只差消费数的行不逐条导出，只计数。
    let (tally_n, rows_n, shorts_n, dump_n) = compare_all(&mgr_n, &py, &conv, &inputs, true);
    write_dump("nbest", &dump_n);
    let mut body_n = format!(
        "# sentence_count=3 sentence_max_count=5\n{}",
        summary("nbest", &tally_n)
    );
    body_n.push_str(HEADER);
    body_n.push_str(&rows_n);
    for s in &shorts_n {
        let _ = writeln!(body_n, "# short_consumed\t{s}");
    }
    std::fs::write(out_dir.join("p1_diff_nbest.tsv"), &body_n).expect("写 p1_diff_nbest.tsv");
    eprint!("{}", summary("nbest", &tally_n));
}

/// `build_dev` 里的小鹤辅码表（`字=码` 每行一条），只给耗时对比用。
struct FlypyAux(std::collections::HashMap<char, Vec<String>>);

impl wind_engine::AuxCharLookup for FlypyAux {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .get(&ch)
            .is_some_and(|v| v.iter().any(|c| c.starts_with(prefix)))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn flypy_aux(dir: &Path) -> Option<FlypyAux> {
    let src = std::fs::read_to_string(dir.join("schemas/aux_code/flypy_full.txt")).ok()?;
    let mut m: std::collections::HashMap<char, Vec<String>> = Default::default();
    for line in src.lines().filter(|l| !l.starts_with('#')) {
        let Some((ch, code)) = line.split_once('=') else {
            continue;
        };
        let mut it = ch.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            m.entry(c).or_default().push(code.trim().to_string());
        }
    }
    Some(FlypyAux(m))
}

/// 击键图开着时，辅码表 `None` 与小鹤辅码表的逐键耗时对比（不罚分，只看建图代价）。
/// 另报两侧引擎首选不同的前缀数，仅供参考。
#[test]
#[ignore = "耗时对比：慢，依赖 build_dev 真实词库与辅码表"]
fn raw_aux_timing() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let Some(table) = flypy_aux(&dir) else {
        eprintln!("跳过：辅码表不存在");
        return;
    };
    let table: std::sync::Arc<dyn wind_engine::AuxCharLookup> = std::sync::Arc::new(table);
    let corpus = corpus(&xiaohe().build_reverse());
    assert!(corpus.len() > 100, "语料只有 {} 句", corpus.len());
    let mut seen = HashSet::new();
    let keys: Vec<String> = corpus
        .iter()
        .flat_map(|(k, _)| (1..=k.len()).map(|n| k[..n].to_string()))
        .filter(|p| seen.insert(p.clone()))
        .collect();
    let mgr = manager(&dir, false);
    let o = |aux: bool| {
        let t = table.clone();
        move || ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: aux.then(|| t.clone()),
            ..Default::default()
        }
    };
    let top = |k: &str, aux: bool| {
        mgr.convert_with_opts("shuangpin", k, LIMIT, o(aux)())
            .candidates
            .first()
            .map(|c| (c.text.clone(), c.consumed_length))
    };
    let differ = keys
        .iter()
        .filter(|k| top(k, false) != top(k, true))
        .count();
    eprintln!(
        "# prefixes={} engine top1 differs (aux None vs flypy)={differ}",
        keys.len()
    );
    for (label, set) in [
        ("all", keys.clone()),
        (
            "even",
            keys.iter().filter(|k| k.len() % 2 == 0).cloned().collect(),
        ),
        (
            "odd",
            keys.iter().filter(|k| k.len() % 2 == 1).cloned().collect(),
        ),
    ] {
        let _ = time_pass(&mgr, &set, o(false));
        let _ = time_pass(&mgr, &set, o(true));
        let (mut a, mut b, mut ak, mut bk) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..TIMING_RUNS {
            let (off, off_k) = time_pass(&mgr, &set, o(false));
            let (on, on_k) = time_pass(&mgr, &set, o(true));
            eprintln!(
                "# [{label}] run {}: aux None mean={:.1}us p95={:.1}us | flypy mean={:.1}us p95={:.1}us | Δ={:+.1}% ({:+.1}us)",
                i + 1,
                off.0,
                off.1,
                on.0,
                on.1,
                (on.0 / off.0 - 1.0) * 100.0,
                on.0 - off.0
            );
            a.push(off);
            b.push(on);
            ak.push(off_k);
            bk.push(on_k);
        }
        let m = |v: &[Pass], f: fn(&Pass) -> f64| v.iter().map(f).sum::<f64>() / v.len() as f64;
        eprintln!(
            "# [{label}] avg: None mean={:.1}us p95={:.1}us | flypy mean={:.1}us p95={:.1}us | best-of max None={:.1}us flypy={:.1}us",
            m(&a, |p| p.0),
            m(&a, |p| p.1),
            m(&b, |p| p.0),
            m(&b, |p| p.1),
            max_of_best(&ak),
            max_of_best(&bk)
        );
    }
}
