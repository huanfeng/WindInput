//! 量化 grammar 对整句分数轴（`log_prob / 字数`）的平移，以及它对跨轴阈值的冲击。
//!
//! 背景：`MIXED_SENTENCE_MIN_LOGP_PER_CHAR = -8.0` 是只有词频时标定的下界，拿整句每字分与固定常量比；
//! grammar 会把整句轴整体平移。现在闸门读的是不含转移分的 `lexical_log_prob`，本探针量的仍是
//! 含转移分的整句分，用于评估同类跨轴比较的风险。结论见 `docs/architecture/candidate-sorting-rules.md`
//! 与 `docs/design/language-model-integration.md` 的「12.1 混合整句质量闸门改看节点分」。
//!
//! ```text
//! WIND_GRAM_MODEL=wanxiang-lts-zh-hans.gram \
//!   cargo test -p wind-engine --test grammar_axis_shift -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;

/// 与 `MIXED_SENTENCE_MIN_LOGP_PER_CHAR` 同值。刻意复制而非引用（那是 crate 私有常量）：
/// 改了那个常量却没跑本探针时，两边脱节本身就是要被发现的事。
const THRESHOLD: f64 = -8.0;

/// 样本必须同时覆盖搭配命中与未命中两类。
///
/// 打分是 `weight × (ln(频次) − baseline)`，baseline = 8.34，而万象常见搭配的 ln 频次在 15~18，
/// 于是命中的整句分上升、未命中的下降。阈值是下界，威胁只来自下降方向；只喂常见句子会得出
/// 「毫无风险」的假结论。
const CASES: &[&str] = &[
    "bzdhaobuhao",
    "wmyiqizou",
    "nhaoma",
    "zdmhaode",
    "womenyiqiquchifan",
    "jintiantianqihenhao",
    "wobuzhidaozenmeban",
    "xiexienidebangzhu",
    "zhegewentihenzhongyao",
    "womenxuyaogengduoshijian",
    "mingtianzaoshangbadianchufa",
    "zhebenshuwoyijingkanwanle",
    "womendejihuayaogaibian",
    "qingdajiazhuyianquan",
    "tamenyijingchufale",
    "nizuotianqunalile",
    "zuotianwanshangxiayu",
    "kanwandianyingyihou",
    "womenzaiyiqigongzuo",
    "zhejianshiqingbunan",
    // ── 以下为「搭配罕见 / 组合生硬」的样本：期望它们在 grammar 下下降 ──
    // 真实用户偶尔会打、但词与词之间没有常见搭配关系的串，本就贴近下界。
    "maowanchengyanjiu",
    "pingguoshoujidiannao",
    "lansewenzitupian",
    "zuoyebenzishuben",
    "chuangkoumenkoulukou",
    "shuibeishuiguodao",
    "yizidengzizhuozi",
    "hongselvsehuangse",
    "shoubiaoyanjingmaozi",
    "gongyuanguangchangjietou",
    "bolibeitiewanmuban",
    "qianbixiangpicailiao",
];

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn mgr(dir: &Path, weight: f64) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.schema.pinyin.grammar.weight = weight;
    cfg.schema.pinyin.grammar.model = std::env::var("WIND_GRAM_MODEL")
        .unwrap_or_else(|_| "wanxiang-lts-zh-hans.gram".to_string());
    EngineManager::new(&cfg, Some(dir))
}

/// 取整句候选的「每字对数概率」。
///
/// 引擎不外露 `log_prob`，但整句候选的 `weight` 是
/// `exp(log_prob/n + ln(DICT_TOTAL))` 的 clamp 结果，可以反解：
/// `log_prob/n = ln(weight) − ln(DICT_TOTAL)`。
///
/// weight 被 clamp 到 `[1, i32::MAX]`，触底/触顶时反解无意义，故一并返回原始 weight 供调用方判断。
fn sentence_logp_per_char(m: &EngineManager, input: &str) -> Option<(String, i32, f64)> {
    let r = m.convert_with("pinyin", input, 10);
    // 整句候选的特征：文本字数 >= 2 且消费了全部输入。这里取首个多字候选作近似——
    // 探针只关心量级与相对变化，不需要精确认定「哪条是整句」。
    let c = r.candidates.iter().find(|c| c.text.chars().count() >= 2)?;
    const DICT_TOTAL_LN: f64 = 19.305_069_1; // ln(242_154_693)
    let n = c.text.chars().count() as f64;
    let logp_per_char = (c.weight as f64).ln() - DICT_TOTAL_LN;
    let _ = n;
    Some((c.text.clone(), c.weight, logp_per_char))
}

#[test]
#[ignore = "探针：依赖 build_dev 真实词库与 .gram 模型"]
fn measure_axis_shift() {
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    };
    if !dir.join("schemas/pinyin/grammar").exists() {
        eprintln!("!!! 跳过：找不到 .gram 模型目录");
        return;
    }
    let w: f64 = std::env::var("WIND_GRAM_WEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5);

    let off = mgr(&dir, 0.0);
    let on = mgr(&dir, w);

    println!("\n=== grammar 对整句分数轴的平移（weight={w}）===");
    println!(
        "{:<30} {:>10} {:>10} {:>9} {:>9} {:>8}",
        "输入", "关weight", "开weight", "关每字", "开每字", "平移"
    );

    let mut shifts = Vec::new();
    let mut crossed = Vec::new();
    for input in CASES {
        let (Some((t0, w0, p0)), Some((t1, w1, p1))) = (
            sentence_logp_per_char(&off, input),
            sentence_logp_per_char(&on, input),
        ) else {
            continue;
        };
        // clamp 触底的样本反解无意义，剔除
        if w0 <= 1 || w1 <= 1 {
            println!("{input:<30} {w0:>10} {w1:>10}   (clamp 触底，跳过)");
            continue;
        }
        let shift = p1 - p0;
        shifts.push(shift);
        println!("{input:<30} {w0:>10} {w1:>10} {p0:>9.2} {p1:>9.2} {shift:>8.2}");
        if p0 >= THRESHOLD && p1 < THRESHOLD {
            crossed.push((*input, t0, t1, p0, p1));
        }
    }

    if !shifts.is_empty() {
        shifts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = shifts.len();
        let mean: f64 = shifts.iter().sum::<f64>() / n as f64;
        println!("\n样本 {n}");
        println!(
            "平移量  最小 {:.2}  中位 {:.2}  最大 {:.2}  平均 {:.2}",
            shifts[0],
            shifts[n / 2],
            shifts[n - 1],
            mean
        );
        println!(
            "阈值 {THRESHOLD}，即开启后距阈值的裕度平均减少 {:.2}",
            -mean
        );
    }

    println!("\n=== 因 grammar 而跌破阈值的样本 ===");
    if crossed.is_empty() {
        println!("  无——当前裕度尚够，但注意上面的平移量就是被吃掉的裕度。");
    } else {
        for (i, t0, t1, p0, p1) in &crossed {
            println!("  {i:<28} 关: {t0} ({p0:.2})  →  开: {t1} ({p1:.2}) 已跌破");
        }
    }
}
