//! 词语联想排序的**离线评测** + **按键路径耗时**实测（论坛 t185）。两条都是测量工具，
//! 默认 `#[ignore]`，不做回归门禁：
//!
//! ```text
//! cargo test -p wind-coordinator --test assoc_eval -- --ignored --nocapture
//! ```
//!
//! # 评测口径（`assoc_rank_eval`）
//!
//! 语料按段抽出汉字串，用 wubi86 词库**正向最大匹配**（2~6 字）切词；每个 ≥2 字的词是一个
//! 事件：上文 = 它的首字，目标 = 整词（「打完一个字，想要的那个词在联想第几位」）。
//! 事件按出现顺序前一半训练、后一半测量：
//!
//! | 配置 | 训练期写入 |
//! |---|---|
//! | 基线 | 无 |
//! | A | 每个训练事件 `record_freq(码, 词)`——用户平时打过这些词 |
//! | A+B | A + `record_assoc_pick(首字, 词)`——且是从联想里选的（**偏乐观**：假设都经联想上屏） |
//!
//! 指标：目标在联想列表（`max_count` = 9）里的 top1 / top3 命中率与 MRR（不在列表记 0）。
//!
//! 语料：`WIND_ASSOC_EVAL_CORPUS`（目录，递归读 .md/.mdx/.txt），默认文档站 `content/`。
//! `WIND_ASSOC_EVAL_N` 限测量事件数（默认 4000）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const SCHEMA: &str = "wubi86";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_path() -> PathBuf {
    data_dir().join("schemas/wubi86/wubi86_jidian.dict.yaml")
}

/// 词 → 最长的那个码（用户打词时敲的全码）。
fn load_dict() -> HashMap<String, String> {
    let text = std::fs::read_to_string(dict_path()).expect("读 wubi86 词库");
    let mut m: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(code), Some(word)) = (it.next(), it.next()) else {
            continue;
        };
        if !code.chars().all(|c| c.is_ascii_lowercase()) || word.is_empty() {
            continue;
        }
        let e = m.entry(word.to_string()).or_default();
        if code.len() > e.len() {
            *e = code.to_string();
        }
    }
    m
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_files(&p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "md" | "mdx" | "txt"))
        {
            out.push(p);
        }
    }
}

/// (上文, 目标词, 目标词的码)
fn events(dict: &HashMap<String, String>) -> Vec<(String, String, String)> {
    let dir = std::env::var("WIND_ASSOC_EVAL_CORPUS").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../WindInputDocs/content"),
        PathBuf::from,
    );
    let mut files = Vec::new();
    collect_files(&dir, &mut files);
    assert!(!files.is_empty(), "语料目录为空：{}", dir.display());
    let mut ev = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if !is_cjk(chars[i]) {
                i += 1;
                continue;
            }
            let mut hit = None;
            for len in (2..=6).rev() {
                if i + len > chars.len() || !chars[i..i + len].iter().all(|&c| is_cjk(c)) {
                    continue;
                }
                let w: String = chars[i..i + len].iter().collect();
                if let Some(code) = dict.get(&w) {
                    hit = Some((w, code.clone(), len));
                    break;
                }
            }
            match hit {
                Some((w, code, len)) => {
                    ev.push((chars[i].to_string(), w, code));
                    i += len;
                }
                None => i += 1,
            }
        }
    }
    ev
}

fn coord(tag: &str, seed: impl FnOnce(&wind_store::Store)) -> Arc<Coordinator> {
    let dir = std::env::temp_dir().join("wind_assoc_eval");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    seed(&store);
    let mut cfg = Config::default();
    cfg.schema.available = vec![SCHEMA.into()];
    cfg.schema.active = SCHEMA.into();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    cfg.input.association.kind = "word".into();
    // 联想按选词次数重排只在调频开着时生效；`Config::default()` 码表调频默认关。
    cfg.schema.codetable.frequency.enabled = true;
    let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    c.prewarm_indexes();
    c
}

#[derive(Default)]
struct Score {
    n: usize,
    top1: usize,
    top3: usize,
    rr: f64,
}

impl Score {
    fn add(&mut self, list: &[String], target: &str) {
        self.n += 1;
        if let Some(p) = list.iter().position(|t| t == target) {
            self.top1 += usize::from(p == 0);
            self.top3 += usize::from(p < 3);
            self.rr += 1.0 / (p + 1) as f64;
        }
    }

    fn line(&self, name: &str) -> String {
        let n = self.n.max(1) as f64;
        format!(
            "{name:<6} n={} top1={:.2}% top3={:.2}% MRR={:.4}",
            self.n,
            100.0 * self.top1 as f64 / n,
            100.0 * self.top3 as f64 / n,
            self.rr / n
        )
    }
}

#[test]
#[ignore = "评测工具：依赖 build_dev 词库与文档站语料，手动跑"]
fn assoc_rank_eval() {
    let dict = load_dict();
    let ev = events(&dict);
    let half = ev.len() / 2;
    let (train, test) = ev.split_at(half);
    let cap: usize = std::env::var("WIND_ASSOC_EVAL_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let test = &test[..test.len().min(cap)];
    println!(
        "事件 {} 条：训练 {}，测量 {}",
        ev.len(),
        train.len(),
        test.len()
    );

    let configs: [(&str, bool, bool); 3] = [
        ("基线", false, false),
        ("A", true, false),
        ("A+B", true, true),
    ];
    for (name, freq, hist) in configs {
        let c = coord(name, |s| {
            for (ctx, w, code) in train {
                if freq {
                    s.record_freq(SCHEMA, code, w).unwrap();
                }
                if hist {
                    s.record_assoc_pick(SCHEMA, ctx, w).unwrap();
                }
            }
        });
        let mut sc = Score::default();
        for (ctx, w, _) in test {
            sc.add(&c.debug_assoc_suggest(ctx), w);
        }
        println!("{}", sc.line(name));
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// 重度用户（19 万条用户词 + 10 万条词频 + 2 万条联想历史）下，上屏后算联想的耗时。
///
/// 两个口径：`debug_assoc_suggest`（纯联想取数：三源 + FREQ 批量点查 + 历史范围扫描），
/// 以及真实按键「敲全码 + 空格上屏」里**空格那一键**的整段处理（含上屏与进联想态）。
#[test]
#[ignore = "性能实测：手动跑"]
fn assoc_key_path_latency_heavy_user() {
    let dict = load_dict();
    let mut words: Vec<(&String, &String)> = dict
        .iter()
        .filter(|(w, _)| w.chars().count() >= 2)
        .collect();
    words.sort();
    // 上文取「作为多字词首字出现过、且有 4 码全码」的单字——即真会有联想的那些字，
    // 按出现次数降序（常用字在前），与真实打字的上文分布同向。
    let singles: Vec<(String, String)> = {
        let mut first: HashMap<char, usize> = HashMap::new();
        for (w, _) in &words {
            *first.entry(w.chars().next().unwrap()).or_default() += 1;
        }
        let mut v: Vec<(usize, String, String)> = first
            .into_iter()
            .filter_map(|(ch, n)| {
                let s = ch.to_string();
                let code = dict.get(&s).filter(|c| c.len() == 4)?.clone();
                Some((n, s, code))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, s, c)| (s, c)).take(1000).collect()
    };
    // 19 万条：词库多字词全收，再用「词 + 词」补足。
    let mut rows: Vec<wind_store::wdict::WordIo> = Vec::new();
    let mut i = 0usize;
    while rows.len() < 190_000 {
        let (w, c) = words[i % words.len()];
        let text = if i < words.len() {
            w.clone()
        } else {
            format!("{w}{}", words[(i * 7919) % words.len()].0)
        };
        rows.push(wind_store::wdict::WordIo {
            code: c.clone(),
            text,
            weight: (i % 1000) as i32,
            ..Default::default()
        });
        i += 1;
    }
    let freq: Vec<wind_store::wdict::FreqIo> = rows
        .iter()
        .step_by(2)
        .take(100_000)
        .enumerate()
        .map(|(k, r)| wind_store::wdict::FreqIo {
            code: r.code.clone(),
            text: r.text.clone(),
            count: (k % 20) as u32 + 1,
            last_used: 1_790_000_000,
        })
        .collect();
    let c = coord("perf", |s| {
        s.import_user_words(SCHEMA, &rows).unwrap();
        s.import_freq_rows(SCHEMA, &freq).unwrap();
        for (k, (w, _)) in words.iter().take(20_000).enumerate() {
            let ctx: String = w.chars().next().unwrap().to_string();
            for _ in 0..(k % 3) + 1 {
                s.record_assoc_pick(SCHEMA, &ctx, w).unwrap();
            }
        }
    });

    let key = |vk: u32| KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    };
    let (mut pure, mut space) = (Vec::new(), Vec::new());
    let mut nonempty = 0usize;
    for (k, (ch, code)) in singles.iter().cycle().take(2000).enumerate() {
        let t = Instant::now();
        let got = c.debug_assoc_suggest(ch);
        pure.push(t.elapsed().as_secs_f64() * 1e3);
        nonempty += usize::from(!got.is_empty());
        if k < 1000 {
            c.handle_key_event(&key(0x1B));
            for b in code.bytes() {
                c.handle_key_event(&key(b.to_ascii_uppercase() as u32));
            }
            let t = Instant::now();
            c.handle_key_event(&key(0x20));
            space.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    println!(
        "纯联想取数 n={} 非空={} P50={:.3}ms P99={:.3}ms max={:.3}ms",
        pure.len(),
        nonempty,
        pct(&mut pure, 0.5),
        pct(&mut pure, 0.99),
        pct(&mut pure, 1.0)
    );
    println!(
        "空格上屏+进联想 n={} P50={:.3}ms P99={:.3}ms max={:.3}ms",
        space.len(),
        pct(&mut space, 0.5),
        pct(&mut space, 0.99),
        pct(&mut space, 1.0)
    );
}

/// 「下一个想要的」评测：事件文件由 `scripts/lm/assoc_eval_prep.py` 生成，每行
/// `前文<TAB>上屏<TAB>后文`。候选条上第 k 格（显示文本）是后文的开头即命中——与切词口径
/// 无关，「们」「们的」都算对，句末给「？」也算。
///
/// ```text
/// WIND_ASSOC_NEXT_EVAL=events_word.tsv,events_char.tsv \
///   cargo test -p wind-coordinator --test assoc_eval assoc_next_eval -- --ignored --nocapture
/// ```
///
/// 配置同手机出厂（智能联想、带标点、9 格），拼音方案（前缀词按拼音词库），空用户库。
/// 每个文件跑两遍：带前文（宿主报了光标前文）与不带（只看刚上屏的）。文件名含 `char`
/// 的按逐字上屏的用户算（运行时是形码方案，见 `Coordinator::assoc_char_wise`）。
#[test]
#[ignore = "评测工具：依赖 build_dev 词库与评测事件文件，手动跑"]
fn assoc_next_eval() {
    let Ok(files) = std::env::var("WIND_ASSOC_NEXT_EVAL") else {
        eprintln!("!!! 跳过：未设 WIND_ASSOC_NEXT_EVAL");
        return;
    };
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.input.default.chinese_mode = true;
    cfg.input.association.kind = "smart".to_string();
    cfg.input.association.punct = true;
    cfg.input.association.max_count = 9;
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    c.prewarm_indexes();

    println!("文件\t前文\t事件\tTop1\tTop3\tTop9\tMRR\t有联想\t标点 Top3\t字词 Top3");
    for file in files.split(',').filter(|f| !f.is_empty()) {
        let text = std::fs::read_to_string(file).expect("读评测事件");
        let events: Vec<(&str, &str, &str)> = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some((it.next()?, it.next()?, it.next()?))
            })
            .collect();
        let name = Path::new(file)
            .file_name()
            .map_or(file.into(), |n| n.to_string_lossy());
        let char_wise = name.contains("char");
        for with_ctx in [true, false] {
            let (mut top1, mut top3, mut top9, mut rr, mut any) = (0, 0, 0, 0.0, 0);
            let (mut punct_n, mut punct3, mut word_n, mut word3) = (0, 0, 0, 0);
            for &(pre, commit, rest) in &events {
                let shown =
                    c.debug_assoc_display(if with_ctx { pre } else { "" }, commit, char_wise);
                let rank = shown
                    .iter()
                    .position(|s| !s.is_empty() && rest.starts_with(s.as_str()));
                any += usize::from(!shown.is_empty());
                if let Some(r) = rank {
                    top1 += usize::from(r == 0);
                    top3 += usize::from(r < 3);
                    top9 += usize::from(r < 9);
                    rr += 1.0 / (r + 1) as f64;
                }
                let hit3 = usize::from(rank.is_some_and(|r| r < 3));
                if rest.chars().next().is_some_and(is_cjk) {
                    word_n += 1;
                    word3 += hit3;
                } else {
                    punct_n += 1;
                    punct3 += hit3;
                }
            }
            let n = events.len().max(1) as f64;
            let p = |k: usize| format!("{:.1}%", k as f64 / n * 100.0);
            let q = |k: usize, d: usize| format!("{:.1}%", k as f64 / d.max(1) as f64 * 100.0);
            println!(
                "{name}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}",
                if with_ctx { "带" } else { "不带" },
                events.len(),
                p(top1),
                p(top3),
                p(top9),
                rr / n,
                p(any),
                q(punct3, punct_n),
                q(word3, word_n),
            );
        }
    }
}
