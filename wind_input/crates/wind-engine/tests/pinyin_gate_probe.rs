//! 「精确整词闸门」（`WIND_GATE_EXACT_SENTENCE`）的定点探针，测量工具，不是门禁：
//! 只打印各定点输入的前 3 候选，供闸门开/关对照。
//!
//! ```text
//! cargo test -p wind-engine --release --test pinyin_gate_probe -- --ignored --nocapture
//! WIND_GATE_EXACT_SENTENCE=1 cargo test ... （同上）
//! ```
//!
//! 挂一个用户词「廉政提醒」（`lianzhengtixing`），看用户词经 step 6 合并后闸门能否据此
//! 抑制整句「李安整体性」。

use std::path::PathBuf;
use std::sync::Arc;

use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

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

#[test]
#[ignore = "定点探针：依赖 build_dev 真实词库，用 --ignored 显式运行"]
fn gate_fixed_point_probe() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };

    let root = std::env::temp_dir().join("wind_pinyin_gate_probe");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    // 用户诉求的原始形态：把「廉政提醒」作为用户词加入。
    store
        .add_user_word("pinyin", "lianzhengtixing", "廉政提醒", 100_000, 0)
        .expect("写入用户词");

    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    let mgr = EngineManager::with_store_override(
        &cfg,
        Some(&dir),
        Some(store),
        Some(root.join("schema_overrides")),
    );

    let gate = std::env::var("WIND_GATE_EXACT_SENTENCE").as_deref() == Ok("1");
    println!(
        "\n=== 定点探针 (WIND_GATE_EXACT_SENTENCE={}) ===",
        if gate { "1 开" } else { "未设置 关" }
    );

    let groups: [(&str, &[&str]); 3] = [
        (
            "原始诉求 + 常规整句",
            &[
                "lianzhengtixing",
                "nihao",
                "woshizhongguoren",
                "jintiantianqizhenhao",
                "zhonghuarenmingongheguo",
            ],
        ),
        (
            "Phase 3/4 已守卫的定点",
            &[
                "xianjiaotongdaxue",
                "qietubiao",
                "guotian",
                "hualong",
                "lianfenxi",
            ],
        ),
        // 自选：整串本身即词典整词，但整句未必更差，闸门会直接掐掉整句，是探测负面影响的靶子。
        // gonghe：恭贺/共贺同码。
        (
            "自选：精确整词 vs 整句",
            &["gonghe", "yijian", "qishi", "yigeren", "sanbaiwushi"],
        ),
    ];

    for (label, inputs) in groups {
        println!("\n--- {label} ---");
        for input in inputs {
            let cands = mgr.convert_with("pinyin", input, 5).candidates;
            let top: Vec<String> = cands
                .iter()
                .take(3)
                .map(|c| format!("{}(w={},句={})", c.text, c.weight, c.is_sentence as u8))
                .collect();
            // 整句所在名次：整句降级后不再恒为 0，须显式定位（`is_sentence` 标记不随降级清除）。
            let sent = cands.iter().position(|c| c.is_sentence);
            let sent_desc = match sent {
                Some(r) => format!(
                    "整句={} rank={} 句={} 降级={}",
                    cands[r].text,
                    r,
                    cands[r].is_sentence as u8,
                    cands[r].is_sentence_demoted as u8
                ),
                None => "整句=无".to_string(),
            };
            println!("  {:<26} {:<58} | {}", input, top.join("  "), sent_desc);
        }
    }
    println!();
}

// ---------------------------------------------------------------- 整句合成探针

/// `pinyin_eval` 的样本输入恒为某词条自身的编码，闸门必然触发且期望答案恰是那个整词，
/// 结构上偏向闸门。本探针补另一半：拼接两个真实词条的编码，期望两词拼接的整句；
/// 闸门只在拼接串碰巧也是词条时触发，那正是它伤人的形态。只打印数字，不做断言。
#[test]
#[ignore = "整句合成探针：依赖 build_dev 真实词库，用 --ignored 显式运行"]
fn gate_sentence_composition_probe() {
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
        .unwrap_or(4000);
    let dump: usize = std::env::var("WIND_PINYIN_EVAL_DUMP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // ---- 读词条（与 pinyin_eval 同源：base.dict.yaml 的数据段）
    let raw = std::fs::read_to_string(dir.join("schemas/pinyin/cn_dicts/base.dict.yaml"))
        .expect("读取 base.dict.yaml");
    let mut words: Vec<(String, String, u64)> = Vec::new();
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
        // 只取 2 字 2 音节的常用词做拼接素材：形态统一，且拼接结果稳定为 4 字 4 音节。
        if chars.len() == 2
            && syls.len() == 2
            && w >= 500
            && chars
                .iter()
                .all(|&c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
            && syls
                .iter()
                .all(|s| s.bytes().all(|b| b.is_ascii_lowercase()))
        {
            words.push((text.to_string(), syls.concat(), w));
        }
    }
    words.sort();
    words.dedup();
    println!("\n可用拼接素材（2字2音节，w>=500）: {} 条", words.len());
    if words.len() < 2 {
        eprintln!("素材不足，跳过");
        return;
    }

    // SplitMix64：与 pinyin_eval 同一个可复现随机源
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

    let gate = std::env::var("WIND_GATE_EXACT_SENTENCE").as_deref() == Ok("1");
    println!(
        "=== 整句合成探针 (seed={}, N={}, WIND_GATE_EXACT_SENTENCE={}) ===",
        seed,
        n,
        if gate { "1 开" } else { "关" }
    );

    let (mut total, mut top1, mut top5, mut mrr) = (0usize, 0usize, 0usize, 0f64);
    // 首选不是整句的样本数。闸门开时含「闸门掐掉了整句」的全部情形，闸门关时是基线量，
    // 两者相减即闸门的实际作用面。
    let mut no_sentence = 0usize;
    let mut no_sentence_top1 = 0usize;
    let mut misses: Vec<String> = Vec::new();

    while total < n {
        let a = &words[(next() % words.len() as u64) as usize];
        let b = &words[(next() % words.len() as u64) as usize];
        let input = format!("{}{}", a.1, b.1);
        let expect = format!("{}{}", a.0, b.0);
        total += 1;

        let cands = mgr.convert_with("pinyin", &input, 10).candidates;
        let rank = cands.iter().position(|c| c.text == expect);
        // 直接观察首选是否被标为整句：闸门触发时整句压根没构造，is_sentence 必为 false。
        if cands.first().map(|c| !c.is_sentence).unwrap_or(true) {
            no_sentence += 1;
            if rank == Some(0) {
                no_sentence_top1 += 1;
            }
        }
        if let Some(r) = rank {
            if r == 0 {
                top1 += 1;
            }
            if r < 5 {
                top5 += 1;
            }
            mrr += 1.0 / (r as f64 + 1.0);
        }
        if rank != Some(0) && misses.len() < dump {
            misses.push(format!(
                "  {:<24} 期望 {:<10} rank={:<5} 首选 {}",
                input,
                expect,
                rank.map(|r| r.to_string()).unwrap_or_else(|| "miss".into()),
                cands.first().map(|c| c.text.as_str()).unwrap_or("")
            ));
        }
    }
    let pct = |h: usize| {
        if total == 0 {
            0.0
        } else {
            h as f64 / total as f64 * 100.0
        }
    };
    println!(
        "样本 {}   top-1 {:.2}%   top-5 {:.2}%   MRR {:.4}",
        total,
        pct(top1),
        pct(top5),
        mrr / total as f64
    );
    println!(
        "首选非整句: {} 条（占 {:.2}%），其中 top-1 命中 {} 条",
        no_sentence,
        pct(no_sentence),
        no_sentence_top1
    );
    for m in &misses {
        println!("{m}");
    }
    println!();
}
