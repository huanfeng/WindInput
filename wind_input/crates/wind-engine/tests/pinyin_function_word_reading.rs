//! 虚词优待必须按**读音**发放，不能只按字形（issue #124）。
//!
//! 背景：`lattice::score_node_inner` 给单字虚词的优待合计 8.0 nat（+2.0 加成、免去
//! −3.0 单字罚、再豁免 3.0 每词罚），相当于 e^8 ≈ 2981 倍词频。旧判据只问字形，于是
//! 多音字的**冷僻读音**白拿这份优待：`会` 读 kuài（会计，w=334）时照样按助动词计价，
//! 把同音位上的「快」（w=29689）压掉 3.5 nat ——「快吃饭」出成「会吃饭」。
//!
//! **必须用真实词库**：内联夹具没有多音字的分读音权重，也没有 `boundary`，测不出本文件
//! 关心的任何东西。词典缺失时自动跳过（同 `pinyin_multipath.rs`）。

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

fn top1(mgr: &EngineManager, input: &str) -> String {
    mgr.convert_with("pinyin", input, 10)
        .candidates
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_default()
}

/// 多音字在**非虚词读音**上不得拿虚词优待，该音位上的高频实词字必须夺回首选。
///
/// 每例都标出「旧行为 → 期望」，其中前两例是 issue #124 的原始复现串。
#[test]
fn test_polyphone_non_function_reading_loses_credit() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    for (input, expect) in [
        ("kuaichifan", "快吃饭"),   // 旧：会吃饭（会 kuai w=334 压过 快 w=29689）
        ("kuaixiuxi", "快休息"),    // 旧：会休息
        ("duyibenshu", "读一本书"), // 旧：都一本书（都 du w=23501 压过 读 w=28867）
        // 以下三例本仓当前已正确，作为同族音位的守门：着 zhao/zhuo、了 liao、换 huan
        ("zhaogongzuo", "找工作"),
        ("zhuozishang", "桌子上"),
        ("huanyifu", "换衣服"),
    ] {
        assert_eq!(top1(&mgr, input), expect, "{input} 首选应为 {expect}");
    }
}

/// **反向守门**：虚词在自己本音上的优待不得被本次改动误伤。
///
/// 这些串全靠 8.0 的虚词优待才拼得出来（虚词自成一词、豁免每词罚），一旦
/// `function_word_readings` 把某个字的本音登记错或漏登记，这里立刻红。
#[test]
fn test_function_word_native_reading_keeps_credit() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    for (input, expect) in [
        ("wohuiqu", "我会去"),             // 会 hui
        ("haiyoushenme", "还有什么"),      // 还 hai + 有 you
        ("womendezhongguo", "我们的中国"), // 我/们/的
        ("zheshiwodeshu", "这是我的书"),   // 这/是/我/的
        ("nihaoma", "你好吗"),             // 残码位不给虚词优待（见 score_node_partial_final）
    ] {
        assert_eq!(top1(&mgr, input), expect, "{input} 首选应为 {expect}");
    }
}
