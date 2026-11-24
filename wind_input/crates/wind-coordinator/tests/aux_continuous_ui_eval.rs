//! 连续辅助码的界面序评测（协调器级，`#[ignore]`）。不是门禁，是测量工具；口径见
//! `docs/design/aux-code-continuous.md` §8 与附录「P2 评测」「P3 界面序」。
//!
//! 引擎序 ≠ 界面序：协调器按消费长度整体重排，还有去重、过滤、串尾一步等后处理。本工具逐键
//! `handle_key_event`，读 `Coordinator` 最终候选列表的首位。语料、击键编码与插辅码的随机序列
//! 同 `wind-engine/tests/aux_continuous_eval.rs`（同种子、同顺序）。
//!
//! 每个（语料 × 布局 × 语法模型开 / 关）四种配置：
//! - `old`：`direct` 关（旧路径）；
//! - `tail`：`direct` 开 + 逃生口 `continuous = false`（旧路径 + 串尾直接辅助）；
//! - `none`：`direct` 开、辅码表为空（击键图、不给辅码表）；
//! - `cont`：`direct` 开、辅码表 `flypy_full`（连续辅助码）。
//!
//! 报 k=0..3 界面首选命中（`cont`），k=0 逐键前缀界面首选与 `old` / `tail` / `none` 不同的条数（分
//! 「首选是串尾一步提上来的」与「引擎出的」两类）并按来源句
//! 归类（整串比整句、前缀看首选是否为来源句的前缀）为新更对 / 新更差 / 不分优劣。
//!
//! ```text
//! cargo test -p wind-coordinator --release --test aux_continuous_ui_eval -- --ignored --nocapture
//! ```
//!
//! 环境变量：`WIND_AUX_UI_OUT`（输出目录，默认 `$TMPDIR/aux_continuous_ui_eval`，同名文件覆写）、
//! `WIND_AUX_UI_LAYOUTS`（默认 `xiaohe,ziranma,mspy`）、`WIND_AUX_UI_GRAMMAR`（默认 `off,on`）、
//! `WIND_AUX_UI_CORPUS`（默认 `a,b`）。逐条表 `ui_<语料>_<布局>_<模型>_{k0,samples}.tsv`，
//! 汇总 `ui_summary.txt`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
const SEED: u64 = 20261118;

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

fn env_list(name: &str, default: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------- 语料（同引擎评测）

fn engine_tests() -> PathBuf {
    root().join("wind_input/crates/wind-engine/tests")
}

fn corpus_a() -> Vec<(String, String)> {
    let src =
        std::fs::read_to_string(engine_tests().join("grammar_sentence_eval.rs")).expect("读语料 A");
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

fn corpus_b() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(engine_tests().join("aux_continuous_corpus_b.txt"))
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
    target: String,
    k: usize,
}

/// `字=码`，只收单字、纯小写码。
fn load_table(dir: &Path) -> HashMap<char, Vec<String>> {
    let src =
        std::fs::read_to_string(dir.join("schemas/aux_code/flypy_full.txt")).expect("读辅码表");
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
    m
}

/// 插法同引擎评测的 `samples`：同种子、同顺序；k=0 不耗随机数。
fn samples(corpus: &[(Vec<String>, String)], table: &HashMap<char, Vec<String>>) -> Vec<Sample> {
    let mut rng = Rng(SEED);
    let mut out = Vec::new();
    for (pairs, hz) in corpus {
        let chars: Vec<char> = hz.chars().collect();
        if chars.iter().any(|c| !table.contains_key(c)) {
            continue;
        }
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
                    let code = &table[&chars[i]][0];
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
                target: hz.clone(),
                k,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- 协调器

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Old,
    /// `direct` 开 + 逃生口：只做串尾直接辅助（今天开着 `direct` 的用户）
    Tail,
    None,
    Cont,
}

/// 本进程的临时目录根（跑完整个删掉）。
fn tmp_root() -> PathBuf {
    std::env::temp_dir().join(format!("wind_aux_ui_{}", std::process::id()))
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = tmp_root().join(tag);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

fn coord(layout: &str, grammar: bool, mode: Mode, tag: &str) -> Arc<Coordinator> {
    let ov = tmp_dir(&format!("{tag}_ov"));
    let mut body = format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n");
    if mode == Mode::Tail {
        body.push_str("[engine.aux_code]\ncontinuous = false\n");
    }
    if mode == Mode::None {
        // 空辅码表：协调器照常走击键图，引擎见表空即不接辅码边（= 引擎评测的 `None`）。
        let empty = ov.join("empty_aux.txt");
        std::fs::write(&empty, "# 空\n").expect("写空辅码表");
        let _ = writeln!(
            body,
            "[engine.aux_code]\nfiles = [{:?}]",
            empty.display().to_string()
        );
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

fn key(c: &Coordinator, vk: u32) {
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

/// 界面候选一条：(文字, 消费数, 是否串尾一步提上来的)。
type Row = (String, usize, bool);

/// 清空后逐键敲入，每键后取界面候选前三。
fn type_tops(c: &Coordinator, input: &str) -> Vec<Vec<Row>> {
    key(c, VK_ESCAPE);
    input
        .bytes()
        .map(|b| {
            // 微软双拼 `;` = ing。
            let vk = if b == b';' {
                wind_keys::keymap::VK_SEMICOLON
            } else {
                VK_A + u32::from(b - b'a')
            };
            key(c, vk);
            c.debug_candidate_consumed().into_iter().take(3).collect()
        })
        .collect()
}

fn last_top3(c: &Coordinator, input: &str) -> Vec<Row> {
    type_tops(c, input).pop().unwrap_or_default()
}

fn text(v: &[Row]) -> Option<&String> {
    v.first().map(|r| &r.0)
}

fn fmt3(v: &[Row]) -> String {
    v.iter()
        .map(|(t, n, d)| format!("{t}/{n}{}", if *d { "*" } else { "" }))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- 主体

fn prefix_ok(top: Option<&String>, src: &str, full: bool) -> bool {
    top.is_some_and(|t| {
        if full {
            t == src
        } else {
            !t.is_empty() && src.starts_with(t.as_str())
        }
    })
}

fn verdict(base_ok: bool, on_ok: bool) -> &'static str {
    match (base_ok, on_ok) {
        (false, true) => "new_better",
        (true, false) => "new_worse",
        _ => "neither",
    }
}

#[test]
#[ignore = "评测：慢，依赖 build_dev 真实词库、辅码表与语法模型"]
fn aux_continuous_ui_eval() {
    if !has_data() {
        eprintln!("跳过：build_dev/data 缺双拼方案 / flypy_full.txt / 词库");
        return;
    }
    let out_dir = std::env::var_os("WIND_AUX_UI_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("aux_continuous_ui_eval"));
    std::fs::create_dir_all(&out_dir).expect("建输出目录");
    let has_gram = data_dir()
        .join("schemas/pinyin/grammar/wanxiang-mini.gram")
        .exists();
    let table = load_table(&data_dir());
    let mut summary = String::new();
    let _ = writeln!(summary, "# seed={SEED}");
    for corpus_id in env_list("WIND_AUX_UI_CORPUS", "a,b") {
        let raw = match corpus_id.as_str() {
            "a" => corpus_a(),
            "b" => corpus_b(),
            other => panic!("未知语料 {other}"),
        };
        for layout in env_list("WIND_AUX_UI_LAYOUTS", "xiaohe,ziranma,mspy") {
            let conv = ShuangpinConverter::new(
                Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
                    .unwrap_or_else(|e| panic!("加载布局 {layout}：{e}")),
            );
            let corpus = encode(&raw, &conv);
            assert!(
                corpus.len() > 100,
                "语料 {corpus_id} / {layout} 只有 {} 句",
                corpus.len()
            );
            let samples = samples(&corpus, &table);
            for gram in env_list("WIND_AUX_UI_GRAMMAR", "off,on") {
                let g = gram == "on";
                if g && !has_gram {
                    eprintln!("跳过模型开：缺 wanxiang-mini.gram");
                    continue;
                }
                let tag = format!("{corpus_id}_{layout}_gram{gram}");
                let old = coord(&layout, g, Mode::Old, &format!("{tag}_old"));
                let tail = coord(&layout, g, Mode::Tail, &format!("{tag}_tail"));
                let none = coord(&layout, g, Mode::None, &format!("{tag}_none"));
                let cont = coord(&layout, g, Mode::Cont, &format!("{tag}_cont"));

                // k=0 逐键前缀（逐句连打，同一前缀只计第一次）
                let mut seen = HashSet::new();
                let mut k0_rows = String::from(
                    "base\torigin\tkeys\tlen\tfull\tsource\tbase_top\ton_top\tverdict\tbase_top3\ton_top3\n",
                );
                // (基线, 首选出处：engine / tail = 串尾一步) → 归类 → 条数
                let mut differ: BTreeMap<(&str, &str), BTreeMap<&str, usize>> = BTreeMap::new();
                let mut n_prefix = 0usize;
                let mut full_top: HashMap<String, [Vec<Row>; 3]> = HashMap::new();
                for (pairs, hz) in &corpus {
                    let keys = pairs.concat();
                    let t_old = type_tops(&old, &keys);
                    let t_tail = type_tops(&tail, &keys);
                    let t_none = type_tops(&none, &keys);
                    let t_cont = type_tops(&cont, &keys);
                    let last = keys.len() - 1;
                    full_top.insert(
                        keys.clone(),
                        [
                            t_old[last].clone(),
                            t_none[last].clone(),
                            t_cont[last].clone(),
                        ],
                    );
                    for i in 0..keys.len() {
                        let k = &keys[..=i];
                        if !seen.insert(k.to_string()) {
                            continue;
                        }
                        n_prefix += 1;
                        let full = i == last;
                        for (bname, base) in [
                            ("old", &t_old[i]),
                            ("tail", &t_tail[i]),
                            ("none", &t_none[i]),
                        ] {
                            let on = &t_cont[i];
                            if text(base) == text(on) {
                                continue;
                            }
                            let v = verdict(
                                prefix_ok(text(base), hz, full),
                                prefix_ok(text(on), hz, full),
                            );
                            let origin = if on.first().is_some_and(|r| r.2) {
                                "tail"
                            } else {
                                "engine"
                            };
                            *differ
                                .entry((bname, origin))
                                .or_default()
                                .entry(v)
                                .or_default() += 1;
                            let _ = writeln!(
                                k0_rows,
                                "{bname}\t{origin}\t{k}\t{}\t{full}\t{hz}\t{}\t{}\t{v}\t{}\t{}",
                                k.len(),
                                text(base).map_or("-", String::as_str),
                                text(on).map_or("-", String::as_str),
                                fmt3(base),
                                fmt3(on)
                            );
                        }
                    }
                }

                // 整串样本：k=0 取逐键连打的末键，k≥1 另敲。
                let mut s_rows = String::from("k\traw\ttarget\tui_top3\thit1\thit3\n");
                // [k] → (条数, 首选, 前三)
                let mut ks = [(0usize, 0usize, 0usize); 4];
                // k=0 的三种配置首选命中 [old, none, cont]
                let mut k0_modes = [0usize; 3];
                for s in &samples {
                    let top3 = if s.k == 0 {
                        let t = &full_top[&s.raw];
                        for (m, top) in t.iter().enumerate() {
                            k0_modes[m] += usize::from(text(top) == Some(&s.target));
                        }
                        t[2].clone()
                    } else {
                        last_top3(&cont, &s.raw)
                    };
                    let hit1 = text(&top3) == Some(&s.target);
                    let hit3 = top3.iter().any(|r| r.0 == s.target);
                    ks[s.k].0 += 1;
                    ks[s.k].1 += usize::from(hit1);
                    ks[s.k].2 += usize::from(hit3);
                    let _ = writeln!(
                        s_rows,
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        s.k,
                        s.raw,
                        s.target,
                        fmt3(&top3),
                        u8::from(hit1),
                        u8::from(hit3)
                    );
                }
                let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
                let _ = writeln!(
                    summary,
                    "[{tag}] sentences={} samples={} prefixes={n_prefix}",
                    corpus.len(),
                    samples.len()
                );
                let _ = writeln!(
                    summary,
                    "[{tag}] k=0 ui top1: old={} ({:.1}%) none={} ({:.1}%) cont={} ({:.1}%)",
                    k0_modes[0],
                    pct(k0_modes[0], ks[0].0),
                    k0_modes[1],
                    pct(k0_modes[1], ks[0].0),
                    k0_modes[2],
                    pct(k0_modes[2], ks[0].0)
                );
                for (k, (n, t1, t3)) in ks.iter().enumerate() {
                    let _ = writeln!(
                        summary,
                        "[{tag}] k={k} n={n} ui top1={t1} ({:.1}%) top3={t3} ({:.1}%)",
                        pct(*t1, *n),
                        pct(*t3, *n)
                    );
                }
                for ((b, o), v) in &differ {
                    let _ = writeln!(
                        summary,
                        "[{tag}] k0 prefix ui-top1 differs vs {b} ({o})={} {v:?}",
                        v.values().sum::<usize>()
                    );
                }
                eprint!(
                    "{}",
                    summary
                        .lines()
                        .filter(|l| l.starts_with(&format!("[{tag}]")))
                        .map(|l| format!("{l}\n"))
                        .collect::<String>()
                );
                std::fs::write(out_dir.join(format!("ui_{tag}_k0.tsv")), &k0_rows)
                    .expect("写 k0 表");
                std::fs::write(out_dir.join(format!("ui_{tag}_samples.tsv")), &s_rows)
                    .expect("写样本表");
            }
        }
    }
    std::fs::write(out_dir.join("ui_summary.txt"), &summary).expect("写汇总");
    eprintln!("# 输出 → {}", out_dir.display());
    let _ = std::fs::remove_dir_all(tmp_root());
}
