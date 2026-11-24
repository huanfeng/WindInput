//! 「整句降级」的风险区测量：精确整词与整句合成直接冲突的那一带。测量工具，不是门禁。
//!
//! `pinyin_eval` 的输入恒为单个词条的编码，只能证明「该赢的赢了」；`pinyin_gate_probe`
//! 拼 4 音节输入几乎不触发降级，只能证明「不该受影响的没受影响」。两者之间的盲区是：
//! 整串恰好也是某词条的编码，但用户要的是两词拼成的整句。降级的代价全落在这里。
//!
//! 样本：随机取一对 1~2 音节、weight ≥ 500 的素材词 `(u, v)`，`input = u.code ++ v.code`；
//! 仅当 `input` 也是某词条 `X` 的完整编码且 `X.text != u.text ++ v.text` 时收入。
//!
//! 样本没有单一正确答案：「u+v」侧命中率偏向不该降级，「X」侧偏向该降级。无偏的读数是
//! 「整句候选掉到第几位」的分布。
//!
//! ```text
//! cargo test -p wind-engine --release --test pinyin_demote_risk -- --ignored --nocapture
//! ```
//! 环境变量：`WIND_PINYIN_EVAL_SEED`（默认 20260721）、`WIND_PINYIN_EVAL_N`（默认 2000）、
//! `WIND_PINYIN_EVAL_DUMP`（默认 25）。

use std::collections::HashMap;
use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

struct Entry {
    text: String,
    code: String,
    syls: usize,
    weight: u64,
}

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

#[test]
#[ignore = "风险区测量：依赖 build_dev 真实词库，用 --ignored 显式运行"]
fn demote_risk_zone_probe() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };

    let seed: u64 = std::env::var("WIND_PINYIN_EVAL_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_260_721);
    let n: usize = std::env::var("WIND_PINYIN_EVAL_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let dump: usize = std::env::var("WIND_PINYIN_EVAL_DUMP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);

    // ---- 读词条
    //
    // 必须读全部词库文件：base 最短是 2 音节，单字在 8105/41448 表里；只读 base 构造不出
    // 「1 音节 + 1 音节」这种最典型的冲突（过+天 → guotian）。
    let mut all: Vec<Entry> = Vec::new();
    for file in [
        "8105.dict.yaml",
        "base.dict.yaml",
        "ext.dict.yaml",
        "others.dict.yaml",
    ] {
        let Ok(raw) = std::fs::read_to_string(dir.join("schemas/pinyin/cn_dicts").join(file))
        else {
            continue;
        };
        let mut in_data = false;
        for line in raw.lines() {
            if !in_data {
                if line.trim() == "..." {
                    in_data = true;
                }
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(text), Some(code)) = (it.next(), it.next()) else {
                continue;
            };
            let w = it
                .next()
                .and_then(|w| w.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let chars: Vec<char> = text.chars().collect();
            let syls: Vec<&str> = code.split_whitespace().collect();
            if chars.is_empty() || syls.is_empty() || chars.len() != syls.len() {
                continue;
            }
            if !chars.iter().copied().all(is_cjk) {
                continue;
            }
            if !syls
                .iter()
                .all(|s| s.bytes().all(|b| b.is_ascii_lowercase()))
            {
                continue;
            }
            all.push(Entry {
                text: text.to_string(),
                code: syls.concat(),
                syls: syls.len(),
                weight: w,
            });
        }
    }

    // code → 该码下权重最高的词条（即「覆盖整串输入的精确整词」代表）
    let mut by_code: HashMap<&str, &Entry> = HashMap::new();
    for e in &all {
        by_code
            .entry(e.code.as_str())
            .and_modify(|cur| {
                if e.weight > cur.weight {
                    *cur = e;
                }
            })
            .or_insert(e);
    }

    // 拼接素材：1~2 音节、weight ≥ 500
    let mats: Vec<&Entry> = all
        .iter()
        .filter(|e| e.syls <= 2 && e.weight >= 500)
        .collect();
    let one = mats.iter().filter(|e| e.syls == 1).count();
    println!(
        "\n词条总数 {}（去码后 {} 个不同编码）；拼接素材（1~2 音节，w>=500）{} 条，其中单音节 {} 条",
        all.len(),
        by_code.len(),
        mats.len(),
        one
    );
    if mats.len() < 2 {
        eprintln!("素材不足，跳过");
        return;
    }

    // SplitMix64（与 pinyin_eval / gate_probe 同一可复现随机源）
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };

    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    let mgr = EngineManager::new(&cfg, Some(&dir));

    println!("=== 风险区探针 (seed={seed}, N={n}) ===");
    println!(
        "样本判据：input = u.code ++ v.code（2~3 音节），且 input 本身也是某词条 X 的完整编码，X.text != u.text ++ v.text"
    );

    let (mut total, mut tries) = (0usize, 0usize);
    // 拼接整句 u+v 一侧
    let (mut c_top1, mut c_top5, mut c_mrr) = (0usize, 0usize, 0f64);
    // 精确整词 X 一侧
    let (mut x_top1, mut x_top5, mut x_mrr) = (0usize, 0usize, 0f64);
    // 整句候选（is_sentence 标记）名次分布
    let mut sent_rank_hist = [0usize; 7]; // 0,1,2,3,4,5..9,缺失
    let mut sent_is_composition = 0usize; // 整句候选恰为 u+v 的样本数
    // 不变量违例：整句之前出现了非精确整词的候选（子短语/前缀补全/模糊）。
    // 用来实测同权并列落到 base_order/natural_order 时会不会把整句甩到普通候选之后。
    let mut invariant_violations = 0usize;
    let mut violation_samples: Vec<String> = Vec::new();
    let mut samples: Vec<String> = Vec::new();

    // 上限保护：条件较苛刻，避免素材耗尽时死循环
    while total < n && tries < n * 4000 {
        tries += 1;
        let u = mats[(next() % mats.len() as u64) as usize];
        let v = mats[(next() % mats.len() as u64) as usize];
        if u.syls + v.syls < 2 || u.syls + v.syls > 3 {
            continue;
        }
        let input = format!("{}{}", u.code, v.code);
        let compo = format!("{}{}", u.text, v.text);
        let Some(x) = by_code.get(input.as_str()) else {
            continue;
        };
        if x.text == compo {
            continue;
        }
        total += 1;

        let cands = mgr.convert_with("pinyin", &input, 10).candidates;
        let c_rank = cands.iter().position(|c| c.text == compo);
        let x_rank = cands.iter().position(|c| c.text == x.text);
        let sent = cands.iter().position(|c| c.is_sentence);

        if let Some(r) = c_rank {
            if r == 0 {
                c_top1 += 1;
            }
            if r < 5 {
                c_top5 += 1;
            }
            c_mrr += 1.0 / (r as f64 + 1.0);
        }
        if let Some(r) = x_rank {
            if r == 0 {
                x_top1 += 1;
            }
            if r < 5 {
                x_top5 += 1;
            }
            x_mrr += 1.0 / (r as f64 + 1.0);
        }
        match sent {
            Some(r) => {
                sent_rank_hist[r.min(5)] += 1;
                if cands[r].text == compo {
                    sent_is_composition += 1;
                }
                // 整句之前只允许是精确整词（码 == 输入且不在下层）
                if let Some(bad) = cands[..r]
                    .iter()
                    .find(|c| !(c.code == input && !c.is_fuzzy && !c.is_prefix && !c.is_partial))
                {
                    invariant_violations += 1;
                    if violation_samples.len() < 10 {
                        violation_samples.push(format!(
                            "  {:<14} 整句 {} rank={} 之前出现 {}(w={}, code={}, fuzzy={} prefix={} partial={})",
                            input,
                            cands[r].text,
                            r,
                            bad.text,
                            bad.weight,
                            bad.code,
                            bad.is_fuzzy as u8,
                            bad.is_prefix as u8,
                            bad.is_partial as u8,
                        ));
                    }
                }
            }
            None => sent_rank_hist[6] += 1,
        }

        if samples.len() < dump {
            samples.push(format!(
                "  {:<14} 精确整词 {:<8} rank={:<5} | 拼接 {:<10} rank={:<5} | 整句 {:<10} rank={:<5} | 首选 {}",
                input,
                x.text,
                x_rank.map(|r| r.to_string()).unwrap_or_else(|| "miss".into()),
                compo,
                c_rank.map(|r| r.to_string()).unwrap_or_else(|| "miss".into()),
                sent.map(|r| cands[r].text.clone()).unwrap_or_else(|| "-".into()),
                sent.map(|r| r.to_string()).unwrap_or_else(|| "无".into()),
                cands.first().map(|c| c.text.as_str()).unwrap_or(""),
            ));
        }
    }

    if total == 0 {
        println!("未采到样本（tries={tries}）");
        return;
    }
    let pct = |h: usize| h as f64 / total as f64 * 100.0;
    println!("\n样本 {total}（尝试 {tries} 次配对）");
    println!(
        "拼接整句 u+v ：top-1 {:.2}%   top-5 {:.2}%   MRR {:.4}",
        pct(c_top1),
        pct(c_top5),
        c_mrr / total as f64
    );
    println!(
        "精确整词  X  ：top-1 {:.2}%   top-5 {:.2}%   MRR {:.4}",
        pct(x_top1),
        pct(x_top5),
        x_mrr / total as f64
    );
    println!("\n整句候选（is_sentence）名次分布：");
    let labels = [
        "rank 0",
        "rank 1",
        "rank 2",
        "rank 3",
        "rank 4",
        "rank>=5",
        "无整句",
    ];
    for (i, l) in labels.iter().enumerate() {
        println!(
            "  {:<8} {:>6} 条  ({:.2}%)",
            l,
            sent_rank_hist[i],
            pct(sent_rank_hist[i])
        );
    }
    println!(
        "  其中整句内容恰为拼接 u+v 的: {} 条 ({:.2}%)",
        sent_is_composition,
        pct(sent_is_composition)
    );
    println!(
        "\n不变量「整句之前只有精确整词」违例: {} 条 ({:.2}%)",
        invariant_violations,
        pct(invariant_violations)
    );
    for v in &violation_samples {
        println!("{v}");
    }
    println!("\n样本明细：");
    for s in &samples {
        println!("{s}");
    }
    println!();
}
