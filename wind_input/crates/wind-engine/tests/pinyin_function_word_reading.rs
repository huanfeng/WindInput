//! 虚词优待必须按**读音**发放，不能只按字形（issue #124）。
//!
//! 背景：`lattice::score_node_inner` 给单字虚词的优待合计 8.0 nat（+2.0 加成、免去
//! −3.0 单字罚、再豁免 3.0 每词罚），相当于 e^8 ≈ 2981 倍词频。旧判据只问字形，于是
//! 多音字的**冷僻读音**白拿这份优待：`会` 读 kuài（会计，w=334）时照样按助动词计价，
//! 把同音位上的「快」（w=29689）压掉 3.5 nat ——「快吃饭」出成「会吃饭」。
//!
//! **必须用真实词库**：内联夹具没有多音字的分读音权重，也没有 `boundary`，测不出本文件
//! 关心的任何东西。词典缺失时跳过（同 `pinyin_multipath.rs`）。
//!
//! ⚠️ **CI 里 `build_dev/` 不入库（`.gitignore`），本文件在 CI 上是静默跳过、计数照绿的**
//! ——issue #124 的回归防线目前只存在于开发机。置 `WIND_REQUIRE_DICT=1` 可把跳过改成
//! 硬失败，供「本应有词库」的环境（本机、部署前自检、将来生成词库的 CI job）扣上闸门。
//! 见 `project_build_dev_data_missing.md` 记的同族坑：那里的判据是耗时，这里连耗时都不
//! 可用（词库走 mmap，真跑也只要 0.06s），只能靠这个显式开关。

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

/// 多音字在**非虚词读音**上不得拿虚词优待，该音位上的高频实词字必须夺回首选。
///
/// 每例都标出「旧行为 → 期望」，其中前两例是 issue #124 的原始复现串。
///
/// ⚠️ **`地`/`di` 刻意没有用例，不是遗漏**：它是本次改动里权重最高的被剥夺者
/// （`地 di` w=117349，掉 8.0 nat），直觉上最该守门，但实测 `dijiagemaidao` /
/// `dicengdegongzuo` / `dixiashi` / `diyiming` 等串**修复前后逐字相同**——di 音位上
/// 「第一名」「地下室」「低价格」这些词典整词本来就赢，单字节点根本没进入竞争。
/// ⇒ **行为无差异的用例不是守门，是噪声**。要给 `地` 补守门，得先找到一个单字「地」
/// 真正参与整句竞争的串；找不到就说明这个音位上本次改动是中性的。
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
        // ★ `得` 是表里唯一一条**两个读音都真实有效**的登记（de/dei 在词库里同为 w=31646）。
        //   `这 => [zhe, zhei]` 与 `那 => [na, nei]` 的第二个读音只存在于 41448 且 w=0，
        //   走 `(0.5/DICT_TOTAL).ln()` 分支后永远排不上去，**写不出能红的用例**——故本组
        //   只锁 `dei`：将来有人「清理冗余读音」删掉它，这一行会红。
        ("wodeiqu", "我得去"), // 得 dei（助动词「必须」）
    ] {
        assert_eq!(top1(&mgr, input), expect, "{input} 首选应为 {expect}");
    }
}
