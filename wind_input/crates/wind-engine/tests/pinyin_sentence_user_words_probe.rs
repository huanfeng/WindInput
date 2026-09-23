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
///
/// 本探针量的就是**出厂路径够不够用**：用户造了词、开了开关，整句认不认。
/// 加 `USER_NODE_BONUS`(+2.0) 之前，答案是「不认，要手工把权重调到 1300」——
/// 那等于这个功能对默认路径上的用户不存在。加成之后 w=1200 那一行就该翻过来，
/// **这一行是本探针的主断言**；它要是退回「有概论吗」，就是加成没生效或被改小了。
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
    // 主断言：**出厂路径**（手动加词 w=1200）就要能翻过来。
    // 拉到 50000 才变说明加成没生效；完全不变说明这条路没接通。
    assert_eq!(
        changed_at,
        Some(1200),
        "出厂加词权重就该让整句改变（USER_NODE_BONUS 的存在意义）"
    );
}

/// 开销：`add_store_nodes` 给**每个跨度**做一次 redb 点查，而这跑在按键路径上。
///
/// 三条整句通路都接了它（step 2 / 2b 混合 / 2c 残码），长串的跨度数是
/// `O(n × max_word_len)`，所以「开 vs 关」的差值是这个功能的真实成本。
#[test]
#[ignore = "依赖 build_dev/data，且是计时用例"]
fn store_node_lookup_cost_on_real_dict() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };
    let db = std::env::temp_dir().join("wind_s2_cost.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(wind_store::Store::open(&db).unwrap());
    // 放一批用户词，模拟用得久了的词库。
    for (code, text, boundary) in [
        ("gailun", "盖伦", 0b1001u64),
        ("jinkesi", "劫克斯", 0b1001001),
        ("yasuo", "亚索", 0b101),
        ("zhaoxin", "赵信", 0b10001),
    ] {
        store
            .add_user_word("pinyin", code, text, 1200, boundary)
            .unwrap();
    }

    let mut cfg_off = Config::load(Some(&dir)).unwrap_or_default();
    cfg_off.schema.pinyin.sentence_uses_user_words = false;
    let mut cfg_on = cfg_off.clone();
    cfg_on.schema.pinyin.sentence_uses_user_words = true;
    let off = EngineManager::with_store(&cfg_off, Some(&dir), Some(store.clone()));
    let on = EngineManager::with_store(&cfg_on, Some(&dir), Some(store));

    // 覆盖三条通路：纯全拼（step 2）、带残码（2c）、含简拼段（2b）。
    let probes = [
        "yougailunma",
        "wojintianhenkaixin",
        "jintiantianqihenhao",
        "yougailunm",
        "bzdgailun",
        "woxiangheniyiqichifan",
    ];
    println!("\n输入                    | 关       | 开       | 差");
    println!("------------------------+----------+----------+--------");
    for p in probes {
        let t = std::time::Instant::now();
        const N: u32 = 20;
        for _ in 0..N {
            let _ = off.convert(p, 100);
        }
        let a = t.elapsed() / N;
        let t = std::time::Instant::now();
        for _ in 0..N {
            let _ = on.convert(p, 100);
        }
        let b = t.elapsed() / N;
        println!(
            "{p:<23} | {a:>8.2?} | {b:>8.2?} | {:>6.2?}",
            b.saturating_sub(a)
        );
        assert!(
            b.as_millis() < 30,
            "{p} 开启后单次 convert {b:?} 超过 30ms —— 这是按键线程上的开销"
        );
    }
}
