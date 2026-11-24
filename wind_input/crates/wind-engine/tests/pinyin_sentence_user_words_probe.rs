//! S2（整句认用户词）的真实词库效果探针：造词前后、开关两态的整句对照。
//!
//! `pinyin_eval` 没有用户数据，只能证「无用户词时零差异」，证不了有用户词时更好，故另建探针。
//!
//! `#[ignore]`：依赖 `build_dev/data`（不随仓库分发）。
//! 跑法：`cargo test -p wind-engine --test pinyin_sentence_user_words_probe -- --ignored --nocapture`

use std::sync::Arc;
use wind_config::Config;
use wind_engine::EngineManager;

/// 「盖伦」场景。
///
/// 「盖伦」在系统词库里本就有（w=237），同码「概论」w=1217；手动加词的出厂权重是 1200
/// （`handle_addword.rs::ADD_WORD_WEIGHT`），仍比「概论」低。本探针量出厂路径够不够用：
/// 主断言是 w=1200 那一行就要让整句改变，退回「概论」说明 `USER_NODE_BONUS` 没生效或被改小。
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
        // w=0 那一行用关闭态跑，即出厂基线。
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
    // 主断言：出厂路径（手动加词 w=1200）就要能翻过来。
    // 拉到 50000 才变说明加成没生效；完全不变说明这条路没接通。
    assert_eq!(
        changed_at,
        Some(1200),
        "出厂加词权重就该让整句改变（USER_NODE_BONUS 的存在意义）"
    );
}

/// 开销：`add_store_nodes` 给每个跨度做一次 redb 点查，跑在按键路径上。三条整句通路
/// （step 2 / 2b 混合 / 2c 残码）都接了它，跨度数是 `O(n × max_word_len)`，「开 vs 关」的差值
/// 就是这个功能的真实成本。
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

/// 临时词「拜城县」+ 纯简拼长串 `bcxrmzf`，真实词库下的位次。
///
/// 打印关 / 开两态的前 15 个候选（带整句名次）及「拜城县」「拜城县人民政府」的位次；
/// 开启态同时开 N-best（露 3 算 5），看输给了谁。
///
/// 这里是引擎层顺序，不是界面顺序：协调器 `candidate_display_order` 还要按消费长度整体重排
/// （`cmp_by_consumed`），只吃前缀的候选在引擎层可排在整串简拼词前，界面上相反。
/// 量界面位次要走 `Coordinator` + `debug_all_candidate_texts`。
#[test]
#[ignore = "依赖 build_dev/data"]
fn temp_word_in_abbrev_sentence_on_real_dict() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };
    let base = Config::load(Some(&dir)).unwrap_or_default();
    let mut cfg_off = base.clone();
    cfg_off.schema.pinyin.sentence_uses_user_words = false;
    let mut cfg_on = base;
    cfg_on.schema.pinyin.sentence_uses_user_words = true;
    cfg_on.schema.pinyin.sentence_count = 3;
    cfg_on.schema.pinyin.sentence_max_count = 5;

    for (label, cfg, learn) in [
        ("关（无临时词）", &cfg_off, false),
        ("关（有临时词）", &cfg_off, true),
        ("开（有临时词，露3算5）", &cfg_on, true),
    ] {
        let db = std::env::temp_dir().join(format!("wind_bcx_probe_{}.redb", learn as u8));
        let _ = std::fs::remove_file(&db);
        let store = Arc::new(wind_store::Store::open(&db).unwrap());
        if learn {
            // 出厂自动学词权重 800（coordinator `LEARN_ADD_WEIGHT`），bai|cheng|xian → 0/3/8
            store
                .learn_temp_word("pinyin", "baichengxian", "拜城县", 800, 0b100001001)
                .unwrap();
        }
        let mgr = EngineManager::with_store(cfg, Some(&dir), Some(store));
        for input in ["bcxrmzf", "bcx", "baichx"] {
            let t = std::time::Instant::now();
            let cands = mgr.convert(input, 300).candidates;
            let cost = t.elapsed();
            let pos = |s: &str| {
                cands
                    .iter()
                    .position(|c| c.text == s)
                    .map_or("无".to_string(), |i| format!("第{}位", i + 1))
            };
            println!(
                "\n[{label}] {input}  ({cost:.2?})  拜城县: {}  拜城县人民政府: {}",
                pos("拜城县"),
                pos("拜城县人民政府")
            );
            for (i, c) in cands.iter().take(15).enumerate() {
                let tag = if c.is_sentence {
                    format!("整句#{}", c.sentence_rank)
                } else if c.meta.is_temp_dict {
                    "临时词".into()
                } else {
                    String::new()
                };
                println!("  {:>2}. {} {tag}", i + 1, c.text);
            }
        }
    }
}

/// 开销：用户词节点的模糊分支（`add_store_nodes` → `add_store_fuzzy_nodes`）。
///
/// 每个跨度 × 至多 4 条切分 × 至多 64 个变体码各一次 store 点查，11 组模糊全开时是 S2 里
/// 最贵的一段。5000 条用户词模拟用久了的词库；4 / 6 / 8 音节各一条，三种配置同进程交替跑
/// 3 轮，免得热身/频率漂移偏向某一态。分支本身的增量要拿去掉该分支的构建另跑对照。
#[test]
#[ignore = "依赖 build_dev/data，且是计时用例（请用 --release）"]
fn fuzzy_store_node_cost_on_real_dict() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };
    let db = std::env::temp_dir().join("wind_s2_fz_cost.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(wind_store::Store::open(&db).unwrap());

    // 5000 条随机多音节词（2~4 音节），音节池偏向探针串里出现的音节，让点查真有命中。
    const POOL: &[&str] = &[
        "cai", "jiu", "duo", "lian", "wo", "ni", "jin", "tian", "ba", "shi", "zhi", "chi", "fan",
        "xiang", "he", "yi", "qi", "hao", "de", "le", "zai", "you", "ren", "min", "zhong", "guo",
        "sheng", "huo", "neng", "li", "lan", "nan", "fang", "hua", "ran", "can", "zan", "san",
        "ying", "xin", "qing", "jian", "yuan", "wang", "chuan",
    ];
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut rows = Vec::with_capacity(5000);
    while rows.len() < 5000 {
        let n = 2 + (next() % 3) as usize;
        let mut code = String::new();
        let mut boundary = 0u64;
        let mut text = String::new();
        for _ in 0..n {
            boundary |= 1 << code.len();
            code.push_str(POOL[(next() % POOL.len() as u64) as usize]);
            text.push(char::from_u32(0x4E00 + (next() % 20000) as u32).unwrap());
        }
        rows.push(wind_store::wdict::WordIo {
            code,
            text,
            weight: 1200,
            count: 0,
            boundary: Some(boundary),
        });
    }
    store.import_user_words("pinyin", &rows).unwrap();
    println!("用户词: {}", store.count_user_words("pinyin").unwrap());

    let base = Config::load(Some(&dir)).unwrap_or_default();
    let mk = |fz: bool, s2: bool| {
        let mut c = base.clone();
        let f = &mut c.schema.pinyin.fuzzy;
        f.enabled = fz;
        f.zh_z = fz;
        f.ch_c = fz;
        f.sh_s = fz;
        f.n_l = fz;
        f.f_h = fz;
        f.r_l = fz;
        f.an_ang = fz;
        f.en_eng = fz;
        f.in_ing = fz;
        f.ian_iang = fz;
        f.uan_uang = fz;
        f.un_ong = fz;
        f.eng_ong = fz;
        c.schema.pinyin.sentence_uses_user_words = s2;
        EngineManager::with_store(&c, Some(&dir), Some(store.clone()))
    };
    let configs = [
        ("出厂", mk(false, false)),
        ("模糊全开+S2", mk(true, true)),
        ("模糊全开,S2关", mk(true, false)),
        ("模糊关+S2", mk(false, true)),
    ];
    let probes = [
        ("4音节", "chaijiuduolian"),
        ("6音节", "wochaijiuduolianba"),
        ("8音节", "nijintianchaijiuduolianba"),
        ("8音节b", "woxiangheniyiqichifan"),
        // ②b 混合整句（简拼段 + 全拼段）
        ("混合2b", "bzdhaobuhao"),
        ("混合2b长", "wojintianbzdchishenme"),
    ];
    const N: u32 = 30;
    for (_, m) in &configs {
        for (_, p) in probes {
            let _ = m.convert(p, 100); // 热身
        }
    }
    for round in 1..=3 {
        println!("\n—— 第 {round} 轮（每格 {N} 次均值）");
        for (label, p) in probes {
            let mut row = format!("{label:<6} {p:<26}");
            for (name, m) in &configs {
                let t = std::time::Instant::now();
                for _ in 0..N {
                    let _ = m.convert(p, 100);
                }
                row += &format!(" | {name} {:>8.2?}", t.elapsed() / N);
            }
            println!("{row}");
        }
    }
}
