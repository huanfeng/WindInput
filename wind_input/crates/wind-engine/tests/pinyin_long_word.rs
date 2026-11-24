//! 长词整句回归：超过 lattice `max_word_len` 的长专名不得被「碎片 + 虚词」切分挤掉首选
//! （如「中华人民共和」+「过」）。词典缺失时自动跳过。

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

fn manager(dir: &std::path::Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    EngineManager::new(&cfg, Some(dir))
}

fn first(mgr: &EngineManager, input: &str) -> String {
    mgr.convert_with("pinyin", input, 10)
        .candidates
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_default()
}

/// 7 音节长专名须整词命中，不得被「碎片 + 虚词」切分挤掉首选。
#[test]
fn test_long_proper_noun_wins_over_fragment_split() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, expect) in [
        ("zhonghuarenmingongheguo", "中华人民共和国"),
        ("renmingongheguo", "人民共和国"),
        ("gongheguo", "共和国"),
        ("zhonghuarenmin", "中华人民"),
    ] {
        assert_eq!(first(&mgr, input), expect, "输入 {} 的首选不对", input);
    }
}

/// 词图上限内的普通精确整词不得被授予 `is_sentence` 身份，否则会被 freq_rerank
/// 锚定在顶部而永久失去词频学习能力。
/// 「共和」自身是 Viterbi 整句故允许带该标记；同码的「恭贺」「共贺」不该被带进去。
#[test]
fn test_short_exact_words_stay_out_of_sentence_anchor() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    let cands = mgr.convert_with("pinyin", "gonghe", 10).candidates;
    assert_eq!(cands.first().map(|c| c.text.as_str()), Some("共和"));

    for c in cands.iter().filter(|c| c.text != "共和") {
        assert!(
            !c.is_sentence,
            "候选「{}」被误标为整句解，将被锚定而失去词频学习",
            c.text
        );
    }
}

/// 超过词图上限（10 音节）的超长词：Viterbi 无法整词命中，词典精确整词仍须排在
/// 拼接整句之前。错误拼接整句的等效词频极低（低频字乘积趋近 clamp 下限），词典整词的
/// 真实词频天然压过它。各例上方注释是错误切分的样子。
///
/// 挑用例须避开「中华人民共和国道路交通安全法」这类：Viterbi 拼出的整句恰好等于词本身，
/// 无论排序机制如何都会通过，是假绿。
#[test]
fn test_over_limit_long_word_falls_back_to_dict_exact() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);

    for (input, expect) in [
        // 罐装动脉…
        (
            "guanzhuangdongmaizhouyangyinghuaxingxinzangbing",
            "冠状动脉粥样硬化性心脏病",
        ),
        // 大不列颠几倍爱尔兰…
        (
            "dabuliedianjibeiaierlanlianhewangguo",
            "大不列颠及北爱尔兰联合王国",
        ),
        // 里昂好的开端…
        (
            "lianghaodekaiduanshichenggongdeyiban",
            "良好的开端是成功的一半",
        ),
        // 塔什库尔干他即可自治县
        ("tashikuergantajikezizhixian", "塔什库尔干塔吉克自治县"),
        // …责任强制保险调理
        (
            "jidongchejiaotongshiguzerenqiangzhibaoxiantiaoli",
            "机动车交通事故责任强制保险条例",
        ),
    ] {
        assert_eq!(first(&mgr, input), expect, "超长词兜底失效: {}", input);
    }
}
