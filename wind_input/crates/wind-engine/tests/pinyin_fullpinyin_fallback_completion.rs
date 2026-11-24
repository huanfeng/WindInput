//! 全拼降级支路（step 6.7）必须与主路径同口径地受词组补全配置约束。
//!
//! 双拼开「允许全拼输入」后，`recall_full_pinyin` 把击键串再当全拼读一遍，自带召回与排序，
//! 是第二条产出通道：主路径上每道补全判据（召回 cap、`completion_extra_syllables` 档位）
//! 这里都得同步，否则同一开关在两条流下表现不一致。
//!
//! 音节数只能按全拼域算：本支路全拼域就是击键域，`started` 可由 `syllables.len()` 与字节长度
//! 直接推出；主路径两域不同，`started_syllables` 必须走双拼域，两处算法混用会静默错配。

use std::path::PathBuf;
use std::sync::Arc;
use wind_config::Config;
use wind_engine::EngineManager;
use wind_store::Store;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 双拼方案 + 允许全拼输入。
fn manager(dir: &std::path::Path, min_syl: u32, max_extra: u32) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    cfg.schema.pinyin.shuangpin.allow_full_pinyin = true;
    cfg.schema.pinyin.completion.min_syllables = min_syl;
    cfg.schema.pinyin.completion.max_extra_syllables = max_extra;
    EngineManager::new(&cfg, Some(dir))
}

/// 同 [`manager`]，但带一条 11 音节的用户词。
fn manager_with_user_word(dir: &std::path::Path, tag: &str, max_extra: u32) -> EngineManager {
    const SYLS: &[&str] = &[
        "qing", "feng", "shu", "ru", "fa", "nei", "ce", "wen", "ti", "fan", "kui",
    ];
    let root = std::env::temp_dir().join(format!("wind_fpfb_uw_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    let mut code = String::new();
    let mut boundary: u64 = 0;
    for s in SYLS {
        boundary |= 1u64 << code.len();
        code.push_str(s);
    }
    store
        .add_user_word("pinyin", &code, USER_WORD, 1000, boundary)
        .expect("写入用户词");

    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    cfg.schema.pinyin.shuangpin.allow_full_pinyin = true;
    cfg.schema.pinyin.completion.min_syllables = 4;
    cfg.schema.pinyin.completion.max_extra_syllables = max_extra;
    EngineManager::with_store_override(&cfg, Some(dir), Some(store), Some(root.join("ov")))
}

const USER_WORD: &str = "清风输入法内测问题反馈";

/// 协调器实际传给引擎的候选上限（`initial_candidate_limit`，拼音恒 300）。
/// 回归必须按这个值断言 —— 超出它的候选会被 `truncate` 丢弃，用户永远看不到。
const COORD_LIMIT: usize = 300;

/// 双拼下用全拼打用户词库长词，必须打得出来。
///
/// 主路径把 `qingfengshurufa` 当双拼码切成另一串音节，命中不到这条词，降级支路是唯一通道。
/// 需要两处配合：支路的用户词带上浮判据（`is_promoted_completion`），且 `cmp_match_layers`
/// 的 `fp_demoted` 按 `eff_prefix` 口径判；后者是首键，只看 `is_prefix` 会把提升过的候选
/// 照样沉底到 300 名之外。
#[test]
fn user_long_word_reachable_via_full_pinyin_fallback() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager_with_user_word(&dir, "reachable", 10);

    // started 5、词 11 音节 ⇒ 距词尾 6 ≤ max_extra 10。
    let r = mgr.convert_with("shuangpin", "qingfengshurufa", COORD_LIMIT);
    let pos = r.candidates.iter().position(|c| c.text == USER_WORD);
    assert!(
        pos.is_some(),
        "双拼+全拼降级下该用户词须在协调器实际上限({COORD_LIMIT})内可见\
         （修复前落在 595/604 位、被 truncate 丢弃 ⇒ 用户完全打不出来）；实际候选 {} 条",
        r.candidates.len()
    );
}

/// 反向对照：`max_extra` 收紧到装不下时，该词允许沉回去。
/// 防止「整个删掉 fp_demoted」这种过度修复也让上一条通过，那会放出支路的低置信补全挤占版面。
#[test]
fn user_long_word_still_sinks_when_max_extra_too_small() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager_with_user_word(&dir, "sink", 2);

    let r = mgr.convert_with("shuangpin", "qingfengshurufa", COORD_LIMIT);
    assert!(
        !r.candidates.iter().any(|c| c.text == USER_WORD),
        "max_extra=2 时距词尾 6 超限，不该上浮、也就不该出现在前 {COORD_LIMIT} 名"
    );
}

/// `beijingd` = bei jing + 残码 d ⇒ started 3。出厂 `min_syllables = 4` 未达门槛，
/// 上限收紧到 started 本身 ⇒ 降级支路也不得给出 4 音节及以上的补全。
#[test]
fn fallback_respects_min_syllables_gate() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, 4, 5);
    let r = mgr.convert_with("shuangpin", "beijingd", 60);
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();

    for over in ["北京大学", "北京地区", "北京大学出版社"] {
        assert!(
            !texts.contains(&over),
            "started=3 < min=4，降级支路不得召回超音节的「{over}」；实际前 12: {:?}",
            &texts[..texts.len().min(12)]
        );
    }
    // 反向：音节数对齐的必须还在，否则「没有超音节候选」可能只是整条支路空了。
    assert!(
        texts.contains(&"北京的") || texts.contains(&"背景的"),
        "3 音节候选应正常召回；实际前 12: {:?}",
        &texts[..texts.len().min(12)]
    );
}

/// 门槛放宽后超音节补全回来，且带上正确的音节数档位。
/// 档位错了候选不会消失，只会与对齐候选同档，是静默的排序退化，故直接断言字段值。
#[test]
fn fallback_tags_completion_extra_syllables() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, 2, 5);
    let r = mgr.convert_with("shuangpin", "beijingd", 60);

    // started = 3（bei jing + 残码 d）
    for (text, want_extra) in [("北京的", 0u8), ("北京大学", 1), ("北京大学出版社", 4)]
    {
        let Some(c) = r.candidates.iter().find(|c| c.text == text) else {
            panic!("「{text}」应在候选中（min=2 已放开门槛）");
        };
        assert_eq!(
            c.completion_extra_syllables,
            want_extra,
            "「{text}」{} 音节、started=3 ⇒ extra 应为 {want_extra}（修复前恒 0）",
            c.boundary.count_ones()
        );
    }
}

/// 无残码的整音节输入同样成立：`nihao` ⇒ started 2。
#[test]
fn fallback_extra_without_trailing_partial() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir, 2, 5);
    let r = mgr.convert_with("shuangpin", "nihao", 60);

    for c in r
        .candidates
        .iter()
        .filter(|c| c.is_prefix && c.boundary != 0)
    {
        let want = c.boundary.count_ones().saturating_sub(2) as u8;
        assert_eq!(
            c.completion_extra_syllables,
            want,
            "「{}」{} 音节、started=2 ⇒ extra 应为 {want}",
            c.text,
            c.boundary.count_ones()
        );
    }
}

/// 任意 `(音节序列, 词)` 的用户词夹具（`manager_with_user_word` 的通用版）。
///
/// 目录名带 pid：并行跑测试时只按 `tag` 区分，同一用例的两个进程会共用 store 并互删目录。
fn manager_with_word(dir: &std::path::Path, tag: &str, syls: &[&str], word: &str) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_fpfb_w_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let store = Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"));
    let mut code = String::new();
    let mut boundary: u64 = 0;
    for s in syls {
        boundary |= 1u64 << code.len();
        code.push_str(s);
    }
    store
        .add_user_word("pinyin", &code, word, 1000, boundary)
        .expect("写入用户词");

    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    cfg.schema.pinyin.shuangpin.allow_full_pinyin = true;
    EngineManager::with_store_override(&cfg, Some(dir), Some(store), Some(root.join("ov")))
}

/// 降级支路的用户词也必须守音节边界对齐：这条路走不到 step 6.3 那道 retain。
///
/// `recall_full_pinyin` 刻意放在 6.3 之后（6.3 的尺子按双拼域算，不能拿去裁全拼候选），
/// 所以只改词库层与主路径时这里会照旧跨音节：`shaixu` 切为 `shai|xu`，`xu` 不该被拉长成
/// `xuan`。只在双拼 + 允许全拼输入下复现，主路径用例抓不到。
#[test]
fn fallback_user_word_respects_syllable_alignment() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager_with_word(&dir, "align", &["shai", "xuan"], "筛选");

    // shaix：`shai` + 残码 `x` ⇒ 对齐位落在残码之前 ⇒ 必须可达。
    let r = mgr.convert_with("shuangpin", "shaix", COORD_LIMIT);
    assert!(
        r.candidates.iter().any(|c| c.text == "筛选"),
        "残码位（shai|x）上用户词应可达；实际前 10={:?}",
        r.candidates
            .iter()
            .take(10)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );

    // shaixu：`shai|xu` 两个完整音节 ⇒ 不得把 `xu` 拉长成 `xuan`。
    let r = mgr.convert_with("shuangpin", "shaixu", COORD_LIMIT);
    assert!(
        !r.candidates.iter().any(|c| c.text == "筛选"),
        "shaixu 的 xu 已是完整音节，降级支路不该把它拉长成 xuan；实际前 10={:?}",
        r.candidates
            .iter()
            .take(10)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );

    // 打完整必须回来，否则上一条可能只是「用户词整个失效」。
    let r = mgr.convert_with("shuangpin", "shaixuan", COORD_LIMIT);
    assert!(
        r.candidates.iter().any(|c| c.text == "筛选"),
        "打完整音节后用户词必须可达"
    );
}
