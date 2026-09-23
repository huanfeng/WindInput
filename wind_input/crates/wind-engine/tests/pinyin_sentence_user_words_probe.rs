//! S2 的**真实词库**效果探针：造词前后、开关两态的整句对照。
//!
//! 为什么单独有这个文件：`pinyin_eval` 只能证「无用户词时逐位零差异」，
//! **证不了有用户词时更好** —— 评测集里没有用户数据（`freq-rerank-model.md:532` 已记过
//! 这个盲区）。效果要另建探针集，`tests/pinyin_gate_probe.rs` 是现成先例。
//!
//! `#[ignore]`：依赖 `build_dev/data`（不随仓库分发）。
//! 跑法：`cargo test -p wind-engine --test pinyin_sentence_user_words_probe -- --ignored --nocapture`

use std::sync::Arc;
use wind_config::Config;
use wind_engine::EngineManager;

/// 真机场景（t134「盖伦」）。
///
/// ⚠️ **「盖伦」在 cn_dicts 里本来就有**（`base.dict.yaml:84220`，w=237），同码的「概论」
/// w=1217 —— 所以这不是「系统库没这个词」，是「它在同码里排第二」。而手动加词的出厂权重
/// 恰好是 1200（`handle_addword.rs::ADD_WORD_WEIGHT`），比「概论」低 17 分。
/// 这正是本探针要量的东西：**默认权重够不够翻盘**。
const INPUTS: &[&str] = &["yougailunma", "gailunhenqiang", "wanyigailun"];

/// 权重梯度：0 = 不加用户词（对照），1200 = 手动加词出厂值，其余为用户手调。
const WEIGHTS: &[i32] = &[0, 1200, 1300, 5000, 50_000];

fn data_dir() -> Option<std::path::PathBuf> {
    let mut d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..4 {
        d.pop();
        let c = d.join("build_dev/data");
        if c.join("schemas/pinyin").is_dir() {
            return Some(c);
        }
    }
    None
}

fn sentence_of(mgr: &EngineManager, input: &str) -> String {
    mgr.convert(input, 100)
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
        .map(|c| c.text)
        .unwrap_or_else(|| "(无整句)".into())
}

#[test]
#[ignore = "依赖 build_dev/data"]
fn user_word_changes_the_sentence_on_real_dict() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };

    let mut cfg_off = Config::load(Some(&dir)).unwrap_or_default();
    cfg_off.schema.pinyin.sentence_uses_user_words = false;
    let mut cfg_on = cfg_off.clone();
    cfg_on.schema.pinyin.sentence_uses_user_words = true;

    println!("\n用户词「盖伦」(gai|lun) 的权重梯度对整句的影响");
    println!("（系统库：概论 w=1217、盖伦 w=237；手动加词出厂 w=1200）\n");
    print!("{:<8}", "用户词w");
    for i in INPUTS {
        print!(" | {i:<16}");
    }
    println!();
    println!("{:-<8}{}", "", "-+-----------------".repeat(INPUTS.len()));

    let mut changed_at: Option<i32> = None;
    let mut baseline: Vec<String> = Vec::new();
    for &w in WEIGHTS {
        let db = std::env::temp_dir().join(format!("wind_s2_probe_{w}.redb"));
        let _ = std::fs::remove_file(&db);
        let store = Arc::new(wind_store::Store::open(&db).unwrap());
        if w > 0 {
            store
                .add_user_word("pinyin", "gailun", "盖伦", w, 0b1001)
                .unwrap();
        }
        // w=0 那一行用**关闭态**跑，正是「出厂什么样」的基线。
        let cfg = if w == 0 { &cfg_off } else { &cfg_on };
        let mgr = EngineManager::with_store(cfg, Some(&dir), Some(store));

        let row: Vec<String> = INPUTS.iter().map(|i| sentence_of(&mgr, i)).collect();
        if baseline.is_empty() {
            baseline = row.clone();
        } else if changed_at.is_none() && row != baseline {
            changed_at = Some(w);
        }
        print!("{w:<8}");
        for cell in &row {
            print!(" | {cell:<16}");
        }
        println!();
    }

    match changed_at {
        Some(w) => println!("\n⇒ 用户词权重达到 {w} 时整句开始改变"),
        None => println!("\n⇒ 梯度内整句始终未变"),
    }
    assert!(
        changed_at.is_some(),
        "梯度拉到 {} 都没改变整句，说明这条路没接通（而不是没赢）",
        WEIGHTS.last().unwrap()
    );
}
