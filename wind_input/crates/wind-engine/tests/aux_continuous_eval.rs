//! 连续辅助码（`ConvertOptions::aux_continuous`）评测。**不是门禁**，是测量工具；设计见
//! `docs/design/aux-code-continuous.md` §6、§9 与附录「P2 评测」。
//!
//! 语料两份：A = `grammar_sentence_eval.rs` 的整句（筛法同 `shuangpin_raw_parity`），B =
//! `tests/aux_continuous_corpus_b.txt`（与 A 不重叠，用来独立验证裕度与 λ）。每句按布局转成击键，
//! 再按 `spike/aux-joint` 的 `build_samples`（同种子、同顺序）在随机 k 个字后插该字首个辅码的
//! 1～2 位，k=0..3。每个（语料 × 布局 × 语法模型开 / 关）报：
//!
//! - k=0..3 引擎首选、前三与界面首选命中；
//! - 难例（辅码表 `None` 时干净串首选错的句子）在 k≥1 上的首选 / 前三修复率；
//! - k=0 逐键前缀上开辅码表后引擎首选 `(文字, 消费数)` 与 `None` 不同的条数，按来源句归类
//!   （整串比整句、前缀看首选是否为来源句前缀）为新更对 / 新更差 / 不分优劣；
//! - 每键耗时（逐前缀各调一次，旧路径 / `None` / 辅码表交替各跑 `WIND_AUX_EVAL_RUNS` 遍，记 loadavg）。
//!   旧路径 = `sp_raw_lattice` 关（不构击键图、无辅码）。
//!
//! ```text
//! cargo test -p wind-engine --release --test aux_continuous_eval -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 环境变量：
//! - `WIND_AUX_EVAL_OUT`：输出目录（默认 `$TMPDIR/aux_continuous_eval`）。目录里已有同名文件时
//!   不覆写，除非 `WIND_AUX_EVAL_OVERWRITE=1`。逐条表 `p2_<语料>_<布局>_<模型>_{k0,samples}.tsv`，
//!   汇总 `p2_summary.txt`，耗时 `p2_timing.txt`（只放输出目录，不入库）。
//! - `WIND_AUX_EVAL_LAYOUTS`（默认 `xiaohe,ziranma,mspy`）、`WIND_AUX_EVAL_GRAMMAR`（默认 `off,on`）、
//!   `WIND_AUX_EVAL_CORPUS`（默认 `a,b`）。
//! - `WIND_AUX_EVAL_RUNS`：计时遍数（默认 3，`0` 不计时）。
//! - `WIND_AUX_EVAL_SKIP_QUALITY=1`：只计时，跳过命中与差异统计（不写逐条表与汇总）。
//! - 带 `--features aux-prof`（或 `aux-prof-fine`）时另跑一组分阶段计时（`wind_engine::prof`），
//!   `None` / 辅码表各 `max(RUNS, 1)` 遍，按每键 µs 写进 `p2_timing.txt`。分阶段计时本身有开销，
//!   只看占比，不与上面的口径数混用。
//! - `WIND_AUX_EVAL_DUMP=<目录>`：另把每条输入开辅码表时的引擎原始候选与整句池（`Debug` 全字段）
//!   写成 `<目录>/<语料>_<布局>_<模型>.txt`，供纯性能改动逐字节对拍。
//!
//! 辅码表恒为 `build_dev` 的 `flypy_full.txt`（辅码与布局无关）。模型开 = `wanxiang-mini.gram`、
//! 权重 0.5（同桌面出厂备好的那组）。

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use wind_candidate::Candidate;
use wind_config::Config;
use wind_engine::pinyin::dag::Dag;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::syllable::SyllableTrie;
use wind_engine::{AuxCharLookup, ConvertOptions, EngineManager};

const LIMIT: usize = 300;
const SEED: u64 = 20261118;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn data_dir() -> Option<PathBuf> {
    let p = root().join("build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
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

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "?".to_string())
}

// ---------------------------------------------------------------- 语料

/// `grammar_sentence_eval.rs` 里的 `("拼音", "汉字")` 行。
fn corpus_a() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/grammar_sentence_eval.rs"),
    )
    .expect("读语料 A");
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

/// `aux_continuous_corpus_b.txt`：`拼音<TAB>汉字`，`#` 起头为注释。
fn corpus_b() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/aux_continuous_corpus_b.txt"),
    )
    .expect("读语料 B");
    src.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .map(|(py, hz)| (py.trim().to_string(), hz.trim().to_string()))
        .filter(|(py, _)| py.bytes().all(|b| b.is_ascii_lowercase()))
        .collect()
}

/// (各音节的击键, 汉字)：去重、≥4 字、最大匹配音节数 = 字数、该布局可编码。
fn encode(raw: &[(String, String)], conv: &ShuangpinConverter) -> Vec<(Vec<String>, String)> {
    let rev = conv.build_reverse();
    let trie = SyllableTrie::new();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (py, hz) in raw {
        if !seen.insert(hz.clone()) {
            continue;
        }
        let n = hz.chars().count();
        let syls = Dag::build(py, &trie).maximum_match();
        if n < 4 || syls.len() != n {
            continue;
        }
        let syls: Vec<&str> = syls.iter().map(String::as_str).collect();
        let Some(keys) = rev.encode_all(&syls) else {
            continue;
        };
        assert_eq!(keys.len(), 2 * n, "{hz}");
        let pairs = (0..n).map(|i| keys[2 * i..2 * i + 2].to_string()).collect();
        out.push((pairs, hz.clone()));
    }
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Sample {
    raw: String,
    clean: String,
    target: String,
    k: usize,
}

/// 插法同 `spike/aux-joint` 的 `build_samples`：同种子、同顺序；k=0 不耗随机数。
fn samples(corpus: &[(Vec<String>, String)], table: &Table) -> Vec<Sample> {
    let mut rng = Rng(SEED);
    let mut out = Vec::new();
    for (pairs, hz) in corpus {
        let chars: Vec<char> = hz.chars().collect();
        if chars.iter().any(|c| !table.0.contains_key(c)) {
            continue;
        }
        let clean = pairs.concat();
        for k in 0..=3usize {
            let mut pos: Vec<usize> = Vec::new();
            while pos.len() < k {
                let p = rng.below(chars.len());
                if !pos.contains(&p) {
                    pos.push(p);
                }
            }
            let mut raw = String::new();
            for (i, kk) in pairs.iter().enumerate() {
                raw.push_str(kk);
                if pos.contains(&i) {
                    let code = &table.0[&chars[i]][0];
                    let len = if code.len() >= 2 && rng.below(2) == 1 {
                        2
                    } else {
                        1
                    };
                    raw.push_str(&code[..len]);
                }
            }
            out.push(Sample {
                raw,
                clean: clean.clone(),
                target: hz.clone(),
                k,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- 辅码表

struct Table(std::collections::HashMap<char, Vec<String>>);

impl AuxCharLookup for Table {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .get(&ch)
            .is_some_and(|v| v.iter().any(|c| c.starts_with(prefix)))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn letter_mask(&self, ch: char) -> u32 {
        self.0.get(&ch).map_or(0, |v| {
            v.iter()
                .fold(0, |m, c| m | wind_engine::engine::aux_code_letter_bit(c))
        })
    }
}

/// `字=码` 每行一条，只收单字、纯小写码（同 `lambda_sweep::load_table`）。
fn load_table(dir: &Path) -> Option<Table> {
    let src = std::fs::read_to_string(dir.join("schemas/aux_code/flypy_full.txt")).ok()?;
    let mut m: std::collections::HashMap<char, Vec<String>> = Default::default();
    for line in src.lines().filter(|l| !l.starts_with('#')) {
        let Some((w, c)) = line.split_once('=') else {
            continue;
        };
        let mut it = w.chars();
        let c = c.trim();
        if let (Some(ch), None) = (it.next(), it.next())
            && !c.is_empty()
            && c.bytes().all(|b| b.is_ascii_lowercase())
        {
            m.entry(ch).or_default().push(c.to_string());
        }
    }
    Some(Table(m))
}

// ---------------------------------------------------------------- 引擎

/// 内置 `shuangpin` 方案经 override 换布局，`grammar` 时挂随包 mini 模型（权重 0.5）。
fn manager(dir: &Path, layout: &str, grammar: bool) -> EngineManager {
    let ov = std::env::temp_dir().join(format!("wind_aux_eval_ov_{layout}"));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).expect("建 override 目录");
    std::fs::write(
        ov.join("shuangpin.toml"),
        format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"),
    )
    .expect("写 override");
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    if grammar {
        cfg.schema.pinyin.grammar.enabled = true;
        cfg.schema.pinyin.grammar.weight = 0.5;
        cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".to_string();
    }
    let mgr = EngineManager::with_store_override(&cfg, Some(dir), None, Some(ov));
    assert_eq!(
        mgr.shuangpin_layout_of("shuangpin"),
        layout,
        "布局 override 未生效"
    );
    mgr
}

fn opts(table: Option<&Arc<dyn AuxCharLookup>>) -> ConvertOptions {
    ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: table.cloned(),
        ..Default::default()
    }
}

/// 计时的三条路径。
#[derive(Clone, Copy)]
enum Route<'a> {
    /// 旧路径：`sp_raw_lattice` 关、无辅码
    Old,
    /// 击键图、辅码表 `None`
    RawNone,
    /// 击键图 + 辅码表
    RawAux(&'a Arc<dyn AuxCharLookup>),
}

impl Route<'_> {
    fn opts(self) -> ConvertOptions {
        match self {
            Route::Old => ConvertOptions::default(),
            Route::RawNone => opts(None),
            Route::RawAux(t) => opts(Some(t)),
        }
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
    /// 引擎序前三 `(文字, 消费数)`
    top3: Vec<(String, usize)>,
    ui_top: Option<String>,
    raw: String,
}

fn run(mgr: &EngineManager, keys: &str, table: Option<&Arc<dyn AuxCharLookup>>) -> Out {
    let r = mgr.convert_with_opts("shuangpin", keys, LIMIT, opts(table));
    let raw = format!("{keys}\t{:?}\t{:?}\n", r.candidates, r.sentence_pool);
    let top3 = r
        .candidates
        .iter()
        .take(3)
        .map(|c| (c.text.clone(), c.consumed_length))
        .collect();
    let ui_top = ui_order(r.candidates, keys)
        .into_iter()
        .next()
        .map(|c| c.text);
    Out { top3, ui_top, raw }
}

impl Out {
    fn top(&self) -> Option<&(String, usize)> {
        self.top3.first()
    }
    fn hit1(&self, t: &str) -> bool {
        self.top().is_some_and(|c| c.0 == t)
    }
    fn hit3(&self, t: &str) -> bool {
        self.top3.iter().any(|c| c.0 == t)
    }
}

fn fmt3(o: &Out) -> String {
    o.top3
        .iter()
        .map(|(t, n)| format!("{t}/{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// P1 双拼转换从 0 起连续的音节数（同 `convert` 主路径的 `sp_syllables`：遇无匹配键对即断）。
fn p1_chain(conv: &ShuangpinConverter, keys: &str) -> usize {
    let r = conv.convert(keys);
    let mut cursor = 0usize;
    let mut n = 0;
    for s in &r.syllables {
        if s.fp_start != cursor {
            break;
        }
        cursor = s.fp_end;
        n += 1;
    }
    n
}

// ---------------------------------------------------------------- 耗时

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn time_pass(mgr: &EngineManager, keys: &[String], path: Route) -> Vec<f64> {
    keys.iter()
        .map(|k| {
            let o = path.opts();
            let t = Instant::now();
            let r = mgr.convert_with_opts("shuangpin", k, LIMIT, o);
            let us = t.elapsed().as_secs_f64() * 1e6;
            std::hint::black_box(r);
            us
        })
        .collect()
}

/// 子集上各遍的 (均值, P95) 平均值，与逐键取各遍最小值后的最大值。
fn timing_stats(runs: &[Vec<f64>], pick: &[usize]) -> (f64, f64, f64) {
    let mut mean = 0.0;
    let mut p95 = 0.0;
    for r in runs {
        let mut v: Vec<f64> = pick.iter().map(|&i| r[i]).collect();
        mean += v.iter().sum::<f64>() / v.len().max(1) as f64;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        p95 += pct(&v, 0.95);
    }
    let n = runs.len() as f64;
    let best_max = pick
        .iter()
        .map(|&i| runs.iter().map(|r| r[i]).fold(f64::INFINITY, f64::min))
        .fold(0.0, f64::max);
    (mean / n, p95 / n, best_max)
}

/// 分阶段计时：`None` / 辅码表各跑 `runs` 遍，按每键 µs（计数按每键次数）写一行。
#[cfg(feature = "aux-prof")]
fn prof_pass(
    out: &mut String,
    tag: &str,
    mgr: &EngineManager,
    keys: &[String],
    table: &Arc<dyn AuxCharLookup>,
    runs: usize,
) {
    use wind_engine::prof::{Counter, Stage};
    for (name, path) in [("None", Route::RawNone), ("aux", Route::RawAux(table))] {
        let _ = wind_engine::prof::take();
        let t = Instant::now();
        for _ in 0..runs {
            let _ = time_pass(mgr, keys, path);
        }
        let total_us = t.elapsed().as_secs_f64() * 1e6;
        let snap = wind_engine::prof::take();
        let per = (keys.len() * runs) as f64;
        let mut line = format!("# [{tag}] prof {name}: total={:.1}", total_us / per);
        for (i, st) in Stage::ALL.iter().enumerate() {
            let _ = write!(
                line,
                " {}={:.1}",
                st.name(),
                snap.stage_ns[i] as f64 / 1e3 / per
            );
        }
        for (i, c) in Counter::ALL.iter().enumerate() {
            let _ = write!(line, " #{}={:.0}", c.name(), snap.counts[i] as f64 / per);
        }
        let _ = writeln!(out, "{line} (us/key, #=count/key)");
    }
}

// ---------------------------------------------------------------- 主体

#[derive(Default)]
struct KStat {
    n: usize,
    top1: usize,
    top3: usize,
    ui: usize,
    /// 辅码表 `None`（辅码字母当普通键）的首选命中
    none_top1: usize,
}

fn verdict(off_ok: bool, on_ok: bool) -> &'static str {
    match (off_ok, on_ok) {
        (false, true) => "new_better",
        (true, false) => "new_worse",
        _ => "neither",
    }
}

/// 前缀首选对不对：整串比整句，前缀看首选是否为来源句的前缀。
fn prefix_ok(top: Option<&(String, usize)>, src: &str, full: bool) -> bool {
    top.is_some_and(|(t, _)| {
        if full {
            t == src
        } else {
            !t.is_empty() && src.starts_with(t.as_str())
        }
    })
}

struct Writer {
    dir: PathBuf,
    overwrite: bool,
}

impl Writer {
    fn write(&self, name: &str, body: &str) {
        let p = self.dir.join(name);
        if p.exists() && !self.overwrite {
            eprintln!(
                "已存在、不覆写（设 WIND_AUX_EVAL_OVERWRITE=1 才写）：{}",
                p.display()
            );
            return;
        }
        std::fs::write(&p, body).unwrap_or_else(|e| panic!("写 {}：{e}", p.display()));
    }
}

#[test]
#[ignore = "评测：慢，依赖 build_dev 真实词库、辅码表与语法模型"]
fn aux_continuous_eval() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let Some(table_src) = load_table(&dir) else {
        eprintln!("跳过：辅码表不存在");
        return;
    };
    let out_dir = std::env::var_os("WIND_AUX_EVAL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("aux_continuous_eval"));
    std::fs::create_dir_all(&out_dir).expect("建输出目录");
    let w = Writer {
        dir: out_dir.clone(),
        overwrite: std::env::var("WIND_AUX_EVAL_OVERWRITE").is_ok_and(|v| v == "1"),
    };
    let dump_dir = std::env::var_os("WIND_AUX_EVAL_DUMP").map(PathBuf::from);
    if let Some(d) = &dump_dir {
        std::fs::create_dir_all(d).expect("建对拍目录");
    }
    let runs: usize = std::env::var("WIND_AUX_EVAL_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let skip_quality = std::env::var("WIND_AUX_EVAL_SKIP_QUALITY").is_ok_and(|v| v == "1");
    let has_gram = dir
        .join("schemas/pinyin/grammar/wanxiang-mini.gram")
        .exists();

    let mut summary = String::new();
    let mut timing = String::new();
    let _ = writeln!(summary, "# out={} seed={SEED}", out_dir.display());
    for corpus_id in env_list("WIND_AUX_EVAL_CORPUS", "a,b") {
        let raw = match corpus_id.as_str() {
            "a" => corpus_a(),
            "b" => corpus_b(),
            other => panic!("未知语料 {other}"),
        };
        for layout in env_list("WIND_AUX_EVAL_LAYOUTS", "xiaohe,ziranma,mspy") {
            let conv = ShuangpinConverter::new(
                Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
                    .unwrap_or_else(|e| panic!("加载布局 {layout}：{e}")),
            );
            let corpus = encode(&raw, &conv);
            // 语料没解析出来时别往下跑，免得写出一份空表。
            assert!(
                corpus.len() > 100,
                "语料 {corpus_id} / {layout} 只有 {} 句",
                corpus.len()
            );
            let samples = samples(&corpus, &table_src);
            let table: Arc<dyn AuxCharLookup> = Arc::new(Table(table_src.0.clone()));
            let mut seen = HashSet::new();
            // (前缀, 来源句, 是否整句)
            let prefixes: Vec<(String, String, bool)> = corpus
                .iter()
                .flat_map(|(p, hz)| {
                    let k = p.concat();
                    (1..=k.len())
                        .map(|n| (k[..n].to_string(), hz.clone(), n == k.len()))
                        .collect::<Vec<_>>()
                })
                .filter(|(p, _, _)| seen.insert(p.clone()))
                .collect();
            for gram in env_list("WIND_AUX_EVAL_GRAMMAR", "off,on") {
                let g = gram == "on";
                if g && !has_gram {
                    eprintln!("跳过模型开：缺 wanxiang-mini.gram");
                    continue;
                }
                let tag = format!("{corpus_id}_{layout}_gram{gram}");
                let mgr = manager(&dir, &layout, g);
                if !skip_quality {
                    let mut dump = String::new();

                    // k=0 逐键前缀
                    let mut k0_rows = String::from(
                        "keys\tlen\tfull\tsource\toff_top\toff_consumed\ton_top\ton_consumed\tverdict\toff_top3\ton_top3\n",
                    );
                    let (mut differ, mut odd_differ) = (0, 0);
                    let mut verdicts = std::collections::BTreeMap::<&str, usize>::new();
                    for (k, hz, full) in &prefixes {
                        let off = run(&mgr, k, None);
                        let on = run(&mgr, k, Some(&table));
                        dump.push_str(&on.raw);
                        if off.top() == on.top() {
                            continue;
                        }
                        differ += 1;
                        odd_differ += usize::from(k.len() % 2 == 1);
                        let v = verdict(
                            prefix_ok(off.top(), hz, *full),
                            prefix_ok(on.top(), hz, *full),
                        );
                        *verdicts.entry(v).or_default() += 1;
                        let f = |o: &Out| {
                            o.top()
                                .map_or("-\t0".to_string(), |(t, n)| format!("{t}\t{n}"))
                        };
                        let _ = writeln!(
                            k0_rows,
                            "{k}\t{}\t{full}\t{hz}\t{}\t{}\t{v}\t{}\t{}",
                            k.len(),
                            f(&off),
                            f(&on),
                            fmt3(&off),
                            fmt3(&on)
                        );
                    }

                    // 整串样本
                    let mut ks: [KStat; 4] = Default::default();
                    // 难例：干净串在 None 下首选错
                    let mut hard_set = HashSet::new();
                    let mut clean_cache = std::collections::HashMap::new();
                    let mut hard: [KStat; 4] = Default::default();
                    let mut s_rows = String::from(
                        "k\traw\ttarget\ton_top3\tui_top\tnone_top\thit1\thit3\thard\tp1_syls\n",
                    );
                    // [k] → (未命中, 其中 P1 音节链不足 2 个)
                    let mut chain_miss = [(0usize, 0usize); 4];
                    for s in &samples {
                        let clean_top: &Option<String> =
                            clean_cache.entry(s.clean.clone()).or_insert_with(|| {
                                run(&mgr, &s.clean, None).top().map(|c| c.0.clone())
                            });
                        let clean_ok = clean_top.as_deref() == Some(s.target.as_str());
                        if !clean_ok {
                            hard_set.insert(s.target.clone());
                        }
                        let on = run(&mgr, &s.raw, Some(&table));
                        dump.push_str(&on.raw);
                        let none = run(&mgr, &s.raw, None);
                        for st in [Some(&mut ks[s.k]), (!clean_ok).then_some(&mut hard[s.k])]
                            .into_iter()
                            .flatten()
                        {
                            st.n += 1;
                            st.top1 += usize::from(on.hit1(&s.target));
                            st.top3 += usize::from(on.hit3(&s.target));
                            st.ui += usize::from(on.ui_top.as_deref() == Some(s.target.as_str()));
                            st.none_top1 += usize::from(none.hit1(&s.target));
                        }
                        let p1 = p1_chain(&conv, &s.raw);
                        if !on.hit1(&s.target) {
                            chain_miss[s.k].0 += 1;
                            chain_miss[s.k].1 += usize::from(p1 < 2);
                        }
                        let _ = writeln!(
                            s_rows,
                            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{p1}",
                            s.k,
                            s.raw,
                            s.target,
                            fmt3(&on),
                            on.ui_top.as_deref().unwrap_or("-"),
                            none.top().map_or("-", |c| c.0.as_str()),
                            u8::from(on.hit1(&s.target)),
                            u8::from(on.hit3(&s.target)),
                            u8::from(!clean_ok)
                        );
                    }

                    // 定点（同 spike）：难例在首选写错的字后补该字首个辅码的 1 / 2 位；字数不同时补在第 1 字后。
                    // [位数] → (条数, 首选, 前三)
                    let mut targeted = [(0usize, 0usize, 0usize); 2];
                    let mut t_rows = String::from("len\traw\ttarget\tclean_top\ton_top3\n");
                    for (pairs, hz) in &corpus {
                        let clean = pairs.concat();
                        let Some(Some(top)) = clean_cache.get(&clean) else {
                            continue;
                        };
                        if top == hz {
                            continue;
                        }
                        let tc: Vec<char> = hz.chars().collect();
                        let oc: Vec<char> = top.chars().collect();
                        let wrong: Vec<usize> = if oc.len() == tc.len() {
                            (0..tc.len()).filter(|&i| tc[i] != oc[i]).collect()
                        } else {
                            vec![0]
                        };
                        for (li, len) in [1usize, 2].into_iter().enumerate() {
                            let mut raw = String::new();
                            for (i, kk) in pairs.iter().enumerate() {
                                raw.push_str(kk);
                                if wrong.contains(&i) {
                                    let code = &table_src.0[&tc[i]][0];
                                    raw.push_str(&code[..len.min(code.len())]);
                                }
                            }
                            let on = run(&mgr, &raw, Some(&table));
                            dump.push_str(&on.raw);
                            targeted[li].0 += 1;
                            targeted[li].1 += usize::from(on.hit1(hz));
                            targeted[li].2 += usize::from(on.hit3(hz));
                            let _ = writeln!(t_rows, "{len}\t{raw}\t{hz}\t{top}\t{}", fmt3(&on));
                        }
                    }
                    let pctf = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
                    let _ = writeln!(
                        summary,
                        "[{tag}] targeted (hard, aux on wrong chars): 1-letter top1={}/{} ({:.1}%) top3={} ({:.1}%) | 2-letter top1={}/{} ({:.1}%) top3={} ({:.1}%)",
                        targeted[0].1,
                        targeted[0].0,
                        pctf(targeted[0].1, targeted[0].0),
                        targeted[0].2,
                        pctf(targeted[0].2, targeted[0].0),
                        targeted[1].1,
                        targeted[1].0,
                        pctf(targeted[1].1, targeted[1].0),
                        targeted[1].2,
                        pctf(targeted[1].2, targeted[1].0)
                    );
                    w.write(&format!("p2_{tag}_targeted.tsv"), &t_rows);
                    let _ = writeln!(
                        summary,
                        "[{tag}] sentences={} samples={} prefixes={} (odd {})",
                        corpus.len(),
                        samples.len(),
                        prefixes.len(),
                        prefixes.iter().filter(|p| p.0.len() % 2 == 1).count()
                    );
                    let _ = writeln!(
                        summary,
                        "[{tag}] k0 prefix engine-top1 differs={differ} (odd {odd_differ}) {:?}",
                        verdicts
                    );
                    for (k, st) in ks.iter().enumerate() {
                        let _ = writeln!(
                            summary,
                            "[{tag}] k={k} n={} top1={} ({:.1}%) top3={} ({:.1}%) ui={} ({:.1}%) | None top1={} ({:.1}%)",
                            st.n,
                            st.top1,
                            pctf(st.top1, st.n),
                            st.top3,
                            pctf(st.top3, st.n),
                            st.ui,
                            pctf(st.ui, st.n),
                            st.none_top1,
                            pctf(st.none_top1, st.n)
                        );
                    }
                    let _ = writeln!(
                        summary,
                        "[{tag}] top1 misses (P1 chain < 2 syllables) k=1 {}({}) k=2 {}({}) k=3 {}({})",
                        chain_miss[1].0,
                        chain_miss[1].1,
                        chain_miss[2].0,
                        chain_miss[2].1,
                        chain_miss[3].0,
                        chain_miss[3].1
                    );
                    let (hn, h1, h3) = hard[1..]
                        .iter()
                        .fold((0, 0, 0), |a, s| (a.0 + s.n, a.1 + s.top1, a.2 + s.top3));
                    let _ = writeln!(
                        summary,
                        "[{tag}] hard sentences={} | k>=1 samples={hn} fixed top1={h1} ({:.1}%) top3={h3} ({:.1}%) | per k top1 {}/{} {}/{} {}/{}",
                        hard_set.len(),
                        pctf(h1, hn),
                        pctf(h3, hn),
                        hard[1].top1,
                        hard[1].n,
                        hard[2].top1,
                        hard[2].n,
                        hard[3].top1,
                        hard[3].n
                    );
                    eprint!(
                        "{}",
                        summary
                            .lines()
                            .rev()
                            .take(8)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .map(|l| format!("{l}\n"))
                            .collect::<String>()
                    );
                    w.write(&format!("p2_{tag}_k0.tsv"), &k0_rows);
                    w.write(&format!("p2_{tag}_samples.tsv"), &s_rows);
                    if let Some(d) = &dump_dir {
                        std::fs::write(d.join(format!("{tag}.txt")), &dump).expect("写对拍文件");
                    }
                }

                // 耗时：只计语料 A 的逐键前缀
                if runs == 0 || corpus_id != "a" {
                    continue;
                }
                let keys: Vec<String> = prefixes.iter().map(|p| p.0.clone()).collect();
                let load0 = loadavg();
                let _ = time_pass(&mgr, &keys, Route::Old);
                let _ = time_pass(&mgr, &keys, Route::RawNone);
                let _ = time_pass(&mgr, &keys, Route::RawAux(&table));
                let (mut olds, mut offs, mut ons) = (Vec::new(), Vec::new(), Vec::new());
                for _ in 0..runs {
                    olds.push(time_pass(&mgr, &keys, Route::Old));
                    offs.push(time_pass(&mgr, &keys, Route::RawNone));
                    ons.push(time_pass(&mgr, &keys, Route::RawAux(&table)));
                }
                let load1 = loadavg();
                let all: Vec<usize> = (0..keys.len()).collect();
                let even: Vec<usize> = all
                    .iter()
                    .copied()
                    .filter(|&i| keys[i].len().is_multiple_of(2))
                    .collect();
                let odd: Vec<usize> = all
                    .iter()
                    .copied()
                    .filter(|&i| keys[i].len() % 2 == 1)
                    .collect();
                let _ = writeln!(
                    timing,
                    "# [{tag}] runs={runs} keys={} loadavg before={load0} after={load1}",
                    keys.len()
                );
                for (name, pick) in [("all", &all), ("even", &even), ("odd", &odd)] {
                    let (mo, po, bo) = timing_stats(&olds, pick);
                    let (m0, p0, b0) = timing_stats(&offs, pick);
                    let (m1, p1, b1) = timing_stats(&ons, pick);
                    let _ = writeln!(
                        timing,
                        "# [{tag}] {name}: old mean={mo:.1}us p95={po:.1}us bestmax={bo:.1}us | None mean={m0:.1}us p95={p0:.1}us bestmax={b0:.1}us | aux mean={m1:.1}us p95={p1:.1}us bestmax={b1:.1}us | aux-None Δmean={:+.1}us ({:+.1}%) Δp95={:+.1}us Δbestmax={:+.1}us | aux-old Δmean={:+.1}us ({:+.1}%) Δp95={:+.1}us Δbestmax={:+.1}us | None-old Δmean={:+.1}us ({:+.1}%) Δp95={:+.1}us Δbestmax={:+.1}us",
                        m1 - m0,
                        (m1 / m0 - 1.0) * 100.0,
                        p1 - p0,
                        b1 - b0,
                        m1 - mo,
                        (m1 / mo - 1.0) * 100.0,
                        p1 - po,
                        b1 - bo,
                        m0 - mo,
                        (m0 / mo - 1.0) * 100.0,
                        p0 - po,
                        b0 - bo
                    );
                }
                #[cfg(feature = "aux-prof")]
                prof_pass(&mut timing, &tag, &mgr, &keys, &table, runs.max(1));
                eprint!(
                    "{}",
                    timing
                        .lines()
                        .rev()
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|l| format!("{l}\n"))
                        .collect::<String>()
                );
            }
        }
    }
    if !skip_quality {
        w.write("p2_summary.txt", &summary);
    }
    if !timing.is_empty() {
        // 耗时随机器负载变，只放输出目录、不入库。
        std::fs::write(out_dir.join("p2_timing.txt"), &timing).expect("写 p2_timing.txt");
    }
    eprintln!("# 输出 → {}", out_dir.display());
}
