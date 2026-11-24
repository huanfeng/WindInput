//! 虚词优待必须按读音发放，不能只按字形。
//!
//! `lattice::score_node_inner` 给单字虚词的优待合计 8.0 nat（约 e^8 倍词频）。只按字形判时，
//! 多音字的冷僻读音也白拿：`会` 读 kuài 时照样按助动词计价，压掉同音位的「快」。
//!
//! 必须用真实词库：内联夹具没有多音字的分读音权重与 `boundary`。词典缺失时跳过；
//! CI 上 `build_dev/` 不入库，本文件静默跳过，置 `WIND_REQUIRE_DICT=1` 可把跳过改成硬失败。

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
    let dict = p.join("schemas/pinyin/cn_dicts/base.dict.yaml");
    if !dict.exists() {
        assert!(
            std::env::var_os("WIND_REQUIRE_DICT").is_none(),
            "WIND_REQUIRE_DICT 已置位，但拼音词库缺失：{}",
            dict.display()
        );
        return None;
    }
    Some(p)
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

/// 多音字在非虚词读音上不得拿虚词优待，该音位上的高频实词字必须夺回首选。
///
/// `地`/`di` 刻意没有用例：di 音位上「第一名」「地下室」等词典整词本来就赢，单字「地」
/// 不进竞争，修复前后行为相同，这类用例是噪声而不是守门。
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
        // 以下三例当前已正确，作为同族音位的守门：着 zhao/zhuo、换 huan
        ("zhaogongzuo", "找工作"),
        ("zhuozishang", "桌子上"),
        ("huanyifu", "换衣服"),
    ] {
        assert_eq!(top1(&mgr, input), expect, "{input} 首选应为 {expect}");
    }
}

/// 反向守门：虚词在自己本音上的优待不得被误伤；`function_word_readings` 登记错或漏登记本音时立刻红。
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
        // `得` 是表里唯一两个读音都真实有效的登记（de/dei 同权）；`这`/`那` 的第二读音
        // w=0 永远排不上，写不出能红的用例。本例防「清理冗余读音」时删掉 dei。
        ("wodeiqu", "我得去"), // 得 dei（助动词「必须」）
    ] {
        assert_eq!(top1(&mgr, input), expect, "{input} 首选应为 {expect}");
    }
}
