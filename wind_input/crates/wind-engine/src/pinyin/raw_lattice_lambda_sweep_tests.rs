use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::dag::Dag;
use super::super::shuangpin::{Layout, ShuangpinConverter};
use super::super::syllable::SyllableTrie;
use crate::engine::AuxCharLookup;
use crate::{ConvertOptions, EngineManager};

struct Table(HashMap<char, Vec<String>>);

impl AuxCharLookup for Table {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .get(&ch)
            .is_some_and(|v| v.iter().any(|c| c.starts_with(prefix)))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn load_table(dir: &Path) -> Option<Table> {
    let src = std::fs::read_to_string(dir.join("schemas/aux_code/flypy_full.txt")).ok()?;
    let mut m: HashMap<char, Vec<String>> = HashMap::new();
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

/// `grammar_sentence_eval.rs` 的 `("拼音", "汉字")` 行。
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

/// (击键, 汉字)：筛法同 `shuangpin_raw_parity` 的 `corpus`；另要求每音节两键。
fn corpus(conv: &ShuangpinConverter) -> Vec<(Vec<String>, String)> {
    corpus_of(conv, sentences())
}

/// 第二份语料 `tests/aux_continuous_corpus_b.txt`（`拼音<TAB>汉字`）。
fn sentences_b() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/aux_continuous_corpus_b.txt"),
    )
    .expect("读语料 B");
    src.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .map(|(py, hz)| (py.trim().to_string(), hz.trim().to_string()))
        .collect()
}

fn corpus_of(conv: &ShuangpinConverter, raw: Vec<(String, String)>) -> Vec<(Vec<String>, String)> {
    let rev = conv.build_reverse();
    let trie = SyllableTrie::new();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (py, hz) in raw {
        if !seen.insert(hz.clone()) {
            continue;
        }
        let n = hz.chars().count();
        let syls = Dag::build(&py, &trie).maximum_match();
        if n < 4 || syls.len() != n {
            continue;
        }
        let syls: Vec<&str> = syls.iter().map(String::as_str).collect();
        let Some(keys) = rev.encode_all(&syls) else {
            continue;
        };
        assert_eq!(keys.len(), 2 * n, "{hz}");
        let pairs = (0..n).map(|i| keys[2 * i..2 * i + 2].to_string()).collect();
        out.push((pairs, hz));
    }
    out
}

/// (击键, 目标, k)：插法同 spike 的 `build_samples`（同种子、同顺序）。
fn samples(corpus: &[(Vec<String>, String)], table: &Table) -> Vec<(String, String, usize)> {
    let mut rng = Rng(20261118);
    let mut out = Vec::new();
    for (pairs, hz) in corpus {
        let chars: Vec<char> = hz.chars().collect();
        if chars.iter().any(|c| !table.0.contains_key(c)) {
            continue;
        }
        for k in 1..=3usize {
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
            out.push((raw, hz.clone(), k));
        }
    }
    out
}

/// `WIND_AUX_GATE_MARGIN=<裕度>` 时覆写本线程的闸门裕度（扫参用）。
fn gate_margin_from_env() {
    if let Some(m) = std::env::var("WIND_AUX_GATE_MARGIN")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        super::set_aux_gate_margin(Some(m));
        eprintln!("# gate margin={m}");
    }
}

/// 界面序首选（同 `pinyin_eval::ui_candidates`）。
fn ui_top(mut c: Vec<wind_candidate::Candidate>, input: &str) -> Option<String> {
    ui_sort(&mut c, input);
    c.into_iter().next().map(|c| c.text)
}

fn ui_sort(c: &mut Vec<wind_candidate::Candidate>, input: &str) {
    wind_candidate::drop_corrected_duplicates(c);
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    wind_candidate::protect_top_from_corrected(c, input.len());
}

/// k=0 逐键前缀上开辅码表后首选 `(文字, 消费数)` 与不给表时不同的前缀，按奇偶、文字是否相同、
/// 首选是否用了辅码分桶；另报 λ 极大（结构性差异）与关掉 `aux_whole` 改写时的差异数。
/// `WIND_AUX_K0_DUMP=<文件>` 时把 λ=2 的差异逐条写成 TSV。
#[test]
#[ignore = "k=0 差异分类：依赖 build_dev 真实词库与辅码表"]
fn k0_classify() {
    let dir = root().join("build_dev/data");
    if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let Some(table) = load_table(&dir) else {
        eprintln!("跳过：辅码表不存在");
        return;
    };
    let conv = ShuangpinConverter::new(
        Layout::from_toml(&root().join("data/schemas/shuangpin/xiaohe.toml")).unwrap(),
    );
    gate_margin_from_env();
    let corpus = corpus(&conv);
    let table: Arc<dyn AuxCharLookup> = Arc::new(table);
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&dir));
    // (前缀, 来源句, 是否整句)
    let mut seen = HashSet::new();
    let mut prefixes: Vec<(String, String, bool)> = Vec::new();
    for (p, hz) in &corpus {
        let k = p.concat();
        for n in 1..=k.len() {
            if seen.insert(k[..n].to_string()) {
                prefixes.push((k[..n].to_string(), hz.clone(), n == k.len()));
            }
        }
    }
    // 首选 (文字, 消费数, 是否用了辅码)
    let run = |keys: &str, aux: bool| {
        let opts = ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: aux.then(|| table.clone()),
            ..Default::default()
        };
        super::take_aux_sentences();
        let c = mgr
            .convert_with_opts("shuangpin", keys, 300, opts)
            .candidates;
        let used = super::take_aux_sentences();
        c.first().map(|c| {
            (
                c.text.clone(),
                c.consumed_length,
                c.is_sentence && used.contains(&c.text),
            )
        })
    };
    let base: Vec<_> = prefixes.iter().map(|(k, _, _)| run(k, false)).collect();
    let key = |t: &Option<(String, usize, bool)>| t.as_ref().map(|t| (t.0.clone(), t.1));
    let differ = |lambda: f64, whole_off: bool| {
        let old = super::set_aux_letter_penalty(lambda);
        let old_w = super::set_aux_whole_off(whole_off);
        let out: Vec<_> = prefixes
            .iter()
            .zip(&base)
            .filter_map(|((k, hz, full), b)| {
                let on = run(k, true);
                (key(&on) != key(b)).then(|| (k.clone(), hz.clone(), *full, b.clone(), on))
            })
            .collect();
        super::set_aux_letter_penalty(old);
        super::set_aux_whole_off(old_w);
        out
    };
    let total = prefixes.len();
    let d2 = differ(2.0, false);
    let d_big = differ(1e6, false);
    let d2_off = differ(2.0, true);
    let d_big_off = differ(1e6, true);
    eprintln!("# prefixes={total}");
    eprintln!("# λ=2 differ={}", d2.len());
    eprintln!("# λ=1e6 differ={}", d_big.len());
    eprintln!("# λ=2 aux_whole off differ={}", d2_off.len());
    eprintln!("# λ=1e6 aux_whole off differ={}", d_big_off.len());
    let odd_total = prefixes.iter().filter(|p| p.0.len() % 2 == 1).count();
    eprintln!("# odd prefixes={odd_total} even={}", total - odd_total);
    for (name, d) in [("λ=2", &d2), ("λ=1e6", &d_big), ("λ=2 whole_off", &d2_off)] {
        // (奇偶, 文字同/异, 首选用辅码) → 条数
        let mut buckets: std::collections::BTreeMap<(&str, &str, &str), usize> = Default::default();
        for (k, _, _, b, on) in d.iter() {
            let parity = if k.len() % 2 == 1 { "odd" } else { "even" };
            let same = match (b, on) {
                (Some(b), Some(o)) if b.0 == o.0 => "same_text",
                _ => "diff_text",
            };
            let aux = if on.as_ref().is_some_and(|o| o.2) {
                "aux_used"
            } else {
                "no_aux"
            };
            *buckets.entry((parity, same, aux)).or_default() += 1;
        }
        for ((p, s, a), n) in buckets {
            eprintln!("# {name} bucket {p} {s} {a} = {n}");
        }
    }
    if let Ok(path) = std::env::var("WIND_AUX_K0_DUMP") {
        let fmt = |t: &Option<(String, usize, bool)>| {
            t.as_ref()
                .map_or("-".to_string(), |t| format!("{}\t{}", t.0, t.1))
        };
        let mut s = String::from(
            "keys\tlen\tfull\tsource\toff_text\toff_consumed\ton_text\ton_consumed\ton_aux\n",
        );
        for (k, hz, full, b, on) in &d2 {
            s.push_str(&format!(
                "{k}\t{}\t{full}\t{hz}\t{}\t{}\t{}\n",
                k.len(),
                fmt(b),
                fmt(on),
                on.as_ref().is_some_and(|o| o.2)
            ));
        }
        std::fs::write(&path, s).expect("写 TSV");
        eprintln!("# dump → {path}");
    }
}

/// 逐条看候选：`WIND_AUX_PROBE=键1,键2` 时打印开 / 关辅码表的前 5 条。
#[test]
#[ignore = "探针：依赖 build_dev 真实词库与辅码表"]
fn k0_probe() {
    let dir = root().join("build_dev/data");
    let (Ok(keys), Some(table)) = (std::env::var("WIND_AUX_PROBE"), load_table(&dir)) else {
        eprintln!("跳过：未给 WIND_AUX_PROBE 或缺辅码表");
        return;
    };
    let table: Arc<dyn AuxCharLookup> = Arc::new(table);
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&dir));
    for k in keys.split(',') {
        for aux in [false, true] {
            let opts = ConvertOptions {
                sp_raw_lattice: true,
                aux_continuous: aux.then(|| table.clone()),
                ..Default::default()
            };
            super::take_aux_sentences();
            let c = mgr.convert_with_opts("shuangpin", k, 300, opts).candidates;
            let used = super::take_aux_sentences();
            let top: Vec<String> = c
                .iter()
                .take(5)
                .map(|c| {
                    format!(
                        "{}/{}{}{}",
                        c.text,
                        c.consumed_length,
                        if c.is_sentence { "/S" } else { "" },
                        if used.contains(&c.text) { "/A" } else { "" }
                    )
                })
                .collect();
            eprintln!(
                "{k} {} {:?} aux_sent={used:?}",
                if aux { "ON " } else { "OFF" },
                top
            );
        }
    }
}

#[test]
#[ignore = "λ 扫参：慢，依赖 build_dev 真实词库与辅码表"]
fn lambda_sweep() {
    let dir = root().join("build_dev/data");
    if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let Some(table) = load_table(&dir) else {
        eprintln!("跳过：辅码表不存在");
        return;
    };
    let conv = ShuangpinConverter::new(
        Layout::from_toml(&root().join("data/schemas/shuangpin/xiaohe.toml")).unwrap(),
    );
    gate_margin_from_env();
    let corpus = corpus(&conv);
    let samples = samples(&corpus, &table);
    let table: Arc<dyn AuxCharLookup> = Arc::new(table);
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&dir));
    let mut seen = HashSet::new();
    let prefixes: Vec<String> = corpus
        .iter()
        .map(|(p, _)| p.concat())
        .flat_map(|k| (1..=k.len()).map(move |n| k[..n].to_string()))
        .filter(|p| seen.insert(p.clone()))
        .collect();
    let run = |keys: &str, aux: bool| {
        let opts = ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: aux.then(|| table.clone()),
            ..Default::default()
        };
        mgr.convert_with_opts("shuangpin", keys, 300, opts)
            .candidates
    };
    let top =
        |c: &[wind_candidate::Candidate]| c.first().map(|c| (c.text.clone(), c.consumed_length));
    let base: Vec<_> = prefixes.iter().map(|k| top(&run(k, false))).collect();
    let n_k = |k| samples.iter().filter(|s| s.2 == k).count();
    eprintln!(
        "# sentences={} prefixes={} samples k1/k2/k3={}/{}/{}",
        corpus.len(),
        prefixes.len(),
        n_k(1),
        n_k(2),
        n_k(3)
    );
    // 无辅码表时的参照（k≥1 的辅码字母只能当音节或残码读）。
    let mut cfgs: Vec<Option<f64>> = vec![None];
    cfgs.extend([1.0, 2.0, 3.0].map(Some));
    for lambda in cfgs {
        let old = lambda.map(super::set_aux_letter_penalty);
        let differ = match lambda {
            None => 0,
            Some(_) => prefixes
                .iter()
                .zip(&base)
                .filter(|(k, b)| top(&run(k, true)) != **b)
                .count(),
        };
        let mut line = format!(
            "# λ={} k=0 top1 differs vs None={differ}/{}",
            lambda.map_or("None".to_string(), |l| l.to_string()),
            prefixes.len()
        );
        for k in 1..=3 {
            let (mut eng, mut ui, mut n) = (0, 0, 0);
            for (raw, target, _) in samples.iter().filter(|s| s.2 == k) {
                let c = run(raw, lambda.is_some());
                n += 1;
                eng += usize::from(c.first().is_some_and(|c| &c.text == target));
                ui += usize::from(ui_top(c, raw).as_ref() == Some(target));
            }
            line.push_str(&format!(" | k={k} engine {eng}/{n} ui {ui}/{n}"));
        }
        eprintln!("{line}");
        if let Some(o) = old {
            super::set_aux_letter_penalty(o);
        }
    }
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

/// 内置 `shuangpin` 方案经 override 换布局；`grammar` 为模型权重时挂 mini 模型。
fn sweep_manager(dir: &Path, layout: &str, grammar: Option<f64>) -> EngineManager {
    let ov = std::env::temp_dir().join(format!(
        "wind_aux_sweep_{}/ov_{layout}_{grammar:?}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("shuangpin.toml"),
        format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"),
    )
    .unwrap();
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    if let Some(w) = grammar {
        cfg.schema.pinyin.grammar.enabled = true;
        cfg.schema.pinyin.grammar.weight = w;
        cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".to_string();
    }
    let mgr = EngineManager::with_store_override(&cfg, Some(dir), None, Some(ov));
    assert_eq!(mgr.shuangpin_layout_of("shuangpin"), layout);
    mgr
}

/// P2 扫参：语料 × 布局 × 模型开 / 关 × λ × 裕度 × 落单放行裕度。每格报 k=0 逐键前缀差异
/// （按来源句归类）、k=1/2/3 引擎首选命中，与闸门分差：k=0 前缀上过了落单判据的辅码整句
/// （全是假读法）的最大分差，k≥1 整串上拼对目标句的辅码整句的最小分差。
///
/// ```text
/// WIND_AUX_SWEEP_CORPUS=a|b WIND_AUX_SWEEP_LAYOUTS=xiaohe,ziranma,mspy WIND_AUX_SWEEP_GRAMMAR=off,on \
/// WIND_AUX_SWEEP_LAMBDAS=1,2,3 WIND_AUX_SWEEP_MARGINS=none,5 WIND_AUX_SWEEP_DANGLING=none,5 \
/// cargo test -p wind-engine --release --lib p2_sweep -- --ignored --nocapture --test-threads=1
/// ```
///
/// `WIND_AUX_SWEEP_MARGINS`：`none` = 出厂两档（模型关 / 开各用各的，默认），数字 = 两档同取该值。
/// `WIND_AUX_SWEEP_DANGLING`：`default` = 出厂，`none` = 落单一律不放行，数字 = 落单时按该裕度比分。
/// `WIND_AUX_SWEEP_UI=1`：首选按界面序（协调器同款排序）取。
/// `WIND_AUX_SWEEP_SE`：末槽辅码整句的闸门裕度（`none` = 不设闸）；
/// `WIND_AUX_SWEEP_GAPS=<文件>`：逐条写出闸门比分（两侧都写）；
/// `WIND_AUX_SWEEP_WEIGHTS`：模型开时的 `grammar.weight`（默认 `0.5`，可列多个）。
#[test]
#[ignore = "P2 扫参：慢，依赖 build_dev 真实词库、辅码表与语法模型"]
fn p2_sweep() {
    let dir = root().join("build_dev/data");
    if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let Some(table) = load_table(&dir) else {
        eprintln!("跳过：辅码表不存在");
        return;
    };
    let nums = |name: &str, default: &str| -> Vec<f64> {
        env_list(name, default)
            .iter()
            .map(|v| v.parse().expect("数字"))
            .collect()
    };
    let lambdas = nums("WIND_AUX_SWEEP_LAMBDAS", "2");
    let opt_list = |name: &str| -> Vec<Option<f64>> {
        env_list(name, "none")
            .iter()
            .map(|v| (v != "none").then(|| v.parse().expect("数字")))
            .collect()
    };
    let margins = opt_list("WIND_AUX_SWEEP_MARGINS");
    // `default` = 出厂，`none` = 一律不放行，数字 = 按该裕度比分。
    let danglings: Vec<Option<Option<f64>>> = env_list("WIND_AUX_SWEEP_DANGLING", "default")
        .iter()
        .map(|v| match v.as_str() {
            "default" => None,
            "none" => Some(None),
            v => Some(Some(v.parse().expect("数字"))),
        })
        .collect();
    let ses = opt_list("WIND_AUX_SWEEP_SE");
    let mut configs = Vec::new();
    for &l in &lambdas {
        for &m in &margins {
            for &d in &danglings {
                for &se in &ses {
                    configs.push((l, m, d, se));
                }
            }
        }
    }
    let gaps_out = std::env::var("WIND_AUX_SWEEP_GAPS").ok();
    let mut gap_rows = String::new();
    let corpus_id = std::env::var("WIND_AUX_SWEEP_CORPUS").unwrap_or_else(|_| "a".into());
    let raw = match corpus_id.as_str() {
        "a" => sentences(),
        "b" => sentences_b(),
        other => panic!("未知语料 {other}"),
    };
    let dump = std::env::var("WIND_AUX_SWEEP_DUMP").ok();
    let ui = std::env::var("WIND_AUX_SWEEP_UI").is_ok_and(|v| v == "1");
    let mut dump_rows = String::new();
    let table_arc: Arc<dyn AuxCharLookup> = Arc::new(Table(table.0.clone()));
    for layout in env_list("WIND_AUX_SWEEP_LAYOUTS", "xiaohe") {
        let conv = ShuangpinConverter::new(
            Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
                .unwrap(),
        );
        let corpus = corpus_of(&conv, raw.clone());
        assert!(corpus.len() > 100, "语料只有 {} 句", corpus.len());
        let samples = samples(&corpus, &table);
        let mut seen = HashSet::new();
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
        let weights = nums("WIND_AUX_SWEEP_WEIGHTS", "0.5");
        let grams: Vec<(String, Option<f64>)> = env_list("WIND_AUX_SWEEP_GRAMMAR", "off")
            .into_iter()
            .flat_map(|g| {
                if g == "on" {
                    weights
                        .iter()
                        .map(|&w| (format!("on{w}"), Some(w)))
                        .collect()
                } else {
                    vec![(g, None)]
                }
            })
            .collect();
        for (gram, weight) in grams {
            let mgr = sweep_manager(&dir, &layout, weight);
            let top = |keys: &str, aux: bool| {
                let opts = ConvertOptions {
                    sp_raw_lattice: true,
                    aux_continuous: aux.then(|| table_arc.clone()),
                    ..Default::default()
                };
                let mut c = mgr
                    .convert_with_opts("shuangpin", keys, 300, opts)
                    .candidates;
                if ui {
                    ui_sort(&mut c, keys);
                }
                c.first().map(|c| (c.text.clone(), c.consumed_length))
            };
            let base: Vec<_> = prefixes.iter().map(|p| top(&p.0, false)).collect();
            let ok = |t: &Option<(String, usize)>, src: &str, full: bool| {
                t.as_ref().is_some_and(|(t, _)| {
                    if full {
                        t == src
                    } else {
                        !t.is_empty() && src.starts_with(t.as_str())
                    }
                })
            };
            for &(lambda, margin, dangling, se) in &configs {
                let old_l = super::set_aux_letter_penalty(lambda);
                let old_m = super::set_aux_gate_margin(margin);
                let old_d = super::set_aux_dangling_margin(dangling);
                let old_s = super::set_aux_se_gate_margin(se);
                let cfg = format!("λ={lambda} margin={margin:?} dangling={dangling:?} se={se:?}");
                let gap_row = |side: &str, keys: &str, g: &super::GateGap, ok: bool| {
                    format!(
                        "{corpus_id}\t{layout}\t{gram}\t{cfg}\t{side}\t{keys}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                        g.kind,
                        g.sentence,
                        u8::from(g.dangling),
                        g.score.map_or("-".into(), |v| format!("{v:.3}")),
                        g.rival.map_or("-".into(), |v| format!("{v:.3}")),
                        g.gap().map_or("-".into(), |v| format!("{v:.3}")),
                        g.chars,
                        g.aux_letters,
                        u8::from(ok)
                    )
                };
                let (mut differ, mut better, mut worse, mut neither) = (0, 0, 0, 0);
                // [kind 'n' / 'e'] → (有对手的条数, 最大分差)
                let mut k0_max = [(0usize, f64::NEG_INFINITY); 2];
                let kind_i = |c: char| usize::from(c == 'e');
                super::take_gate_gaps();
                for ((k, hz, full), b) in prefixes.iter().zip(&base) {
                    let on = top(k, true);
                    for g in super::take_gate_gaps() {
                        if let Some(v) = g.gap() {
                            let m = &mut k0_max[kind_i(g.kind)];
                            m.0 += 1;
                            m.1 = m.1.max(v);
                        }
                        if gaps_out.is_some() {
                            gap_rows.push_str(&gap_row("k0", k, &g, false));
                        }
                    }
                    if on == *b {
                        continue;
                    }
                    differ += 1;
                    let v = match (ok(b, hz, *full), ok(&on, hz, *full)) {
                        (false, true) => {
                            better += 1;
                            "new_better"
                        }
                        (true, false) => {
                            worse += 1;
                            "new_worse"
                        }
                        _ => {
                            neither += 1;
                            "neither"
                        }
                    };
                    if dump.is_some() {
                        let f = |t: &Option<(String, usize)>| {
                            t.as_ref()
                                .map_or("-\t0".to_string(), |t| format!("{}\t{}", t.0, t.1))
                        };
                        dump_rows.push_str(&format!(
                            "{corpus_id}\t{layout}\t{gram}\t{cfg}\t{k}\t{hz}\t{}\t{}\t{v}\n",
                            f(b),
                            f(&on)
                        ));
                    }
                }
                let mut line = format!(
                    "# corpus={corpus_id} layout={layout} gram={gram} {cfg} | k0 differ={differ}/{} better={better} worse={worse} neither={neither} | k0 false S_n rival={} max={:.2} S_e rival={} max={:.2}",
                    prefixes.len(),
                    k0_max[0].0,
                    k0_max[0].1,
                    k0_max[1].0,
                    k0_max[1].1
                );
                // k≥1：拼对目标句的辅码整句的最小分差（按 S_n / S_e 分开）。
                let mut legit_min = [(0usize, f64::INFINITY); 2];
                for k in 1..=3 {
                    let (mut hit, mut n) = (0, 0);
                    for (raw, target, _) in samples.iter().filter(|s| s.2 == k) {
                        n += 1;
                        super::take_gate_gaps();
                        hit += usize::from(top(raw, true).is_some_and(|t| &t.0 == target));
                        for g in super::take_gate_gaps() {
                            let legit = &g.sentence == target;
                            if legit && let Some(v) = g.gap() {
                                let m = &mut legit_min[kind_i(g.kind)];
                                m.0 += 1;
                                m.1 = m.1.min(v);
                            }
                            if gaps_out.is_some() {
                                gap_rows.push_str(&gap_row(&format!("k{k}"), raw, &g, legit));
                            }
                        }
                    }
                    line.push_str(&format!(" | k={k} {hit}/{n}"));
                }
                line.push_str(&format!(
                    " | legit S_n rival={} min={:.2} S_e rival={} min={:.2}",
                    legit_min[0].0, legit_min[0].1, legit_min[1].0, legit_min[1].1
                ));
                eprintln!("{line}");
                super::set_aux_letter_penalty(old_l);
                super::set_aux_gate_margin(old_m);
                super::set_aux_dangling_margin(old_d);
                super::set_aux_se_gate_margin(old_s);
            }
        }
    }
    if let Some(p) = dump {
        std::fs::write(&p, dump_rows).expect("写 TSV");
        eprintln!("# dump → {p}");
    }
    if let Some(p) = gaps_out {
        std::fs::write(&p, gap_rows).expect("写分差 TSV");
        eprintln!("# gaps → {p}");
    }
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("wind_aux_sweep_{}", std::process::id())),
    );
}
