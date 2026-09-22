//! 混输的英文子引擎必须是**借来的那一个**，不是自己另建的一份。
//!
//! 英文引擎带一张词组分词索引，大小随用户英文词库规模走——靶机 18 万条时每份 12.7 MB。
//! 从前 `build_engine` 的 mixed 分支写着 `Self::build_engine("english", …)`，于是进程里
//! 躺两份：`engines["english"]`（英文方案自身 / 英文候选混入 / 临英 / 快捷输入英文）一份，
//! 混输引擎内部一份。启动日志里两条「英文词组分词：后台预热完成」就是它。
//!
//! 判据落在**可观测的副作用**上而不是指针相等：混输若是借的，就必然走
//! `ensure_loaded("english")`，`engines` 表里就会留下 english 这一条；若是自己内联建的，
//! 表里查无此方案。`is_loaded` 是公共 API，不必为测试开后门。
//!
//! ⚠️ 词库缺失时静默跳过（同 `english_phrase_index.rs` 的约定）。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

const MIXED: &str = "wubi86_pinyin";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn fixtures_present(dir: &std::path::Path) -> bool {
    dir.join("schemas/english.schema.toml").exists()
        && dir.join("schemas/english").is_dir()
        && dir.join(format!("schemas/{MIXED}.schema.toml")).exists()
}

fn manager(enable_english: bool) -> Option<EngineManager> {
    let dir = data_dir();
    if !fixtures_present(&dir) {
        eprintln!("跳过：缺少英文方案或混输方案");
        return None;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec![MIXED.into(), "english".into()];
    cfg.schema.active = MIXED.into();
    cfg.schema.mix.enable_english = enable_english;
    Some(EngineManager::new(&cfg, Some(&dir)))
}

/// ★ 开着 `enable_english` 建混输 ⇒ 英文方案必须进 `engines` 表（= 混输借的是它）。
///
/// 反向验证（变异）：把 mixed 分支的 `english_provider.and_then(|f| f())` 换回
/// `Self::build_engine("english", …)`，本用例立刻红——内联构造从不入表。
#[test]
fn a_mixed_engine_borrows_the_shared_english_engine() {
    let Some(mgr) = manager(true) else { return };
    // `EngineManager::new` 已同步建好活跃方案（即混输），这一行只是把它写明白。
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");

    assert!(
        mgr.is_loaded("english"),
        "混输的英文子引擎该是 engines[\"english\"] 那一个；它不在表里说明混输又自己\
         建了一份（每份都带一整张词组分词索引）"
    );
}

/// ★ 关着 `enable_english` 时一次都不许去建英文引擎。
///
/// 通道做成回调而不是直接传 `Arc` 就是为了这个：英文引擎要读盘、建索引，为一个关着的
/// 开关付这笔钱，等于把省下来的内存又还回去。
#[test]
fn a_mixed_engine_does_not_touch_english_when_the_switch_is_off() {
    let Some(mgr) = manager(false) else { return };
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");

    assert!(
        !mgr.is_loaded("english"),
        "enable_english 关着却把英文引擎建了出来——回调的懒被谁求值掉了"
    );
}
