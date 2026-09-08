//! 逐键输入英文词时候选**不得时有时无**。
//!
//! 真机报障：拼音方案下打 `windows`，过程中有时有候选、有时没有；`github` 同样。
//!
//! 根因假设：只收精确命中后，中间前缀里只有恰好成词的那几个才出候选——
//! `win`/`wind` 是词、`windo` 不是、`window`/`windows` 又是。而拼音侧对这种串一条中文
//! 候选也给不出（`wi` 不成音节），于是英文一断档整个候选窗就空掉。五笔下码表仍在出
//! 候选，这个闪烁被掩盖，所以只有拼音方案报得出来。

use std::path::PathBuf;
use wind_candidate::CandidateSource;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready(dir: &std::path::Path) -> bool {
    dir.join("schemas/pinyin.schema.toml").exists()
        && dir.join("schemas/english/en.dict.yaml").exists()
}

fn pinyin_mgr(dir: &std::path::Path, english: bool) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.schema.pinyin.english_merge.enable = english;
    EngineManager::new(&cfg, Some(dir))
}

/// 逐前缀里「一条候选都没有」的那些位置。
fn gaps(mgr: &EngineManager, word: &str) -> Vec<String> {
    walk(mgr, word)
        .into_iter()
        .filter(|(_, total, _)| *total == 0)
        .map(|(p, _, _)| p)
        .collect()
}

/// 逐前缀统计：(总候选, 其中英文)。
fn walk(mgr: &EngineManager, word: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    for n in 1..=word.len() {
        let p = &word[..n];
        let r = mgr.convert(p, 50);
        let en = r
            .candidates
            .iter()
            .filter(|c| c.source == CandidateSource::English)
            .count();
        out.push((p.to_string(), r.candidates.len(), en));
    }
    out
}

/// ★ 本功能**不得新增断档**。
///
/// 判据是「开着时的空窗位置 ⊆ 关着时的空窗位置」，而不是「每一步都有候选」——
/// 后者做不到也不该做：`wi` 两字母低于最小触发长度、拼音也解释不了，
/// 关掉英文混入时它同样是空的，那是既有行为，不是本功能的账。
///
/// 用差集当判据的好处是**基线自证**：不必由谁来断言「`wi` 空是可以接受的」，
/// 关掉开关跑一遍就知道。将来最小触发长度改了、拼音召回改了，这条判据自动跟着走。
#[test]
fn english_merge_adds_no_new_gaps() {
    let dir = data_dir();
    if !ready(&dir) {
        eprintln!("跳过：pinyin 方案或英文库不存在");
        return;
    }
    let off = pinyin_mgr(&dir, false);
    let on = pinyin_mgr(&dir, true);

    for word in ["windows", "github"] {
        eprintln!("\n── {word}（开启英文混入）──");
        for (p, total, en) in walk(&on, word) {
            eprintln!("  {p:<10} 候选 {total:<3} 英文 {en}");
        }
        let base = gaps(&off, word);
        let now = gaps(&on, word);
        eprintln!("  关闭时空窗 {base:?} / 开启时空窗 {now:?}");

        let added: Vec<&String> = now.iter().filter(|p| !base.contains(p)).collect();
        assert!(
            added.is_empty(),
            "{word}: 开启英文混入后**新增**空窗于 {added:?}——逐键输入时候选窗会闪烁。\n\
             （真机现场：`windows` 打到 `windo`、`github` 打到 `gith` 时候选窗空一下）"
        );
        // 前提自证：开着时该比关着时候选更多，否则上面的差集可能只是因为两边都没东西。
        assert!(
            now.len() < base.len() || base.is_empty(),
            "{word}: 开启后空窗数没减少（关 {} / 开 {}），本用例可能测不到东西",
            base.len(),
            now.len()
        );
    }
}
