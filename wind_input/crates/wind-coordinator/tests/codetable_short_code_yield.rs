//! 出简让全的端到端验证（设计见 docs/design/codetable-short-code-yields-full.md）。
//!
//! 单元测试（`short_code_yield` 模块内）证明判定函数本身；本文件证明用户入口上真的打得出来：
//! 记录沿途累积、判据在真实候选链上成立、让位作用在显示序上。
//!
//! 档位统一走方案级 override（`override_with_level`）设，别用全局配置：方案级 `Some(_)` 恒覆盖
//! 全局，哪天出厂方案声明了这一项，用全局设的档位会被静默覆盖、整组假绿。
//! `factory_wubi86_does_not_yield` 是唯一不压方案值的用例，守「出厂即关」。
//!
//! 三条用例须合看：`full_code_yields_to_word`（主用例，`wqiy` 首选「你」→「仰泳」）、
//! `disabled_keeps_dictionary_order`（反向对照，证明词库原序「你」在前）、
//! `second_level_shortcode_yields_at_level_two`（档位边界，`>=` 写成 `>` 时只有它红）。
//!
//! 候选调整优先族四条也须合看：`pinned_char_keeps_the_full_code_top`（置顶即停手）、
//! `store_without_pin_still_yields`（带 store 不写规则仍让位）、
//! `pinning_another_candidate_stops_the_yield_for_the_whole_code`（停整码而非只赦免首条）、
//! `hiding_a_candidate_is_not_a_reorder`（隐藏不算排序）。这一族必须用
//! `new_headless_with_store`：shadow 规则存在 store 里，`new_headless` 的 store 是 `None`。
//!
//! 词典缺失时自动跳过；`build_dev/data` 不存在时整族静默跳过而计数照绿（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 逐键按下——必须逐键：让位的判据来自沿途各级简码位的首选记录，
/// 直接把缓冲设成全码的写法会让记录全空，于是恒不让位。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 全局配置不设档位——出厂全局是 0（关），各用例的档位由方案级 override 给出。
fn wubi_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 把档位写进方案级 override（`schema_overrides/wubi86.toml`），而不是全局配置。
///
/// override 层深合并在方案文件之后，是唯一压得住方案值的落点（见文件头）。
/// 每个用例一个目录（并发写同一文件会撕裂），返回值直接喂 `new_headless_with_override`。
fn override_with_level(tag: &str, level: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_scy_ov_{tag}"));
    std::fs::create_dir_all(&dir).expect("建 override 目录失败");
    std::fs::write(
        dir.join("wubi86.toml"),
        format!(
            "[engine.codetable]
short_code_yield_level = {level}
"
        ),
    )
    .expect("写 override 失败");
    dir
}

fn candidates_for(level: usize, code: &str) -> Option<Vec<String>> {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return None;
    }
    let ov = override_with_level(&format!("{level}_{code}"), level);
    let coord = Coordinator::new_headless_with_override(wubi_config(), Some(&d), Some(ov));
    press(&coord, code);
    Some(coord.debug_all_candidate_texts())
}

/// 出厂守门：什么都不配（全局出厂 0 + 无 override）时，wubi86 不让位。
///
/// 唯一不经 override 的用例：缺了它，给 wubi86 方案补一行 `short_code_yield_level = 3`
/// 不会有测试变红。config 侧对应守门是 `wubi86_schema_does_not_declare_short_code_yield`。
/// 顺带钉住词库原序「你」在前。
#[test]
fn factory_wubi86_does_not_yield() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = Coordinator::new_headless_with_override(wubi_config(), Some(&d), None);
    press(&coord, "wqiy");
    let all = coord.debug_all_candidate_texts();
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("你"),
        "出厂应不让位（全局 0，且出厂方案刻意不声明这一项），实际候选: {head:?}"
    );
}

/// 主用例：「你」的二简是 `wq`，故全码 `wqiy` 的首选让给词。
#[test]
fn full_code_yields_to_word() {
    let Some(all) = candidates_for(3, "wqiy") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("仰泳"),
        "有简码的字应把全码首选让给词，实际候选: {head:?}"
    );
    assert!(
        all.iter().any(|t| t == "你"),
        "让的只是位次，字不得被赶出列表，实际候选: {head:?}"
    );
}

/// 让位的字沉到本码所有候选之后，不是降一位。
///
/// `dddd` 是现成的多候选现场：大 / 大厦 / 硕大 / 磕磕碰碰。若实现写成「与第一个词交换」，
/// 「大」会停在第 2 位而本用例会红。
#[test]
fn the_yielding_char_sinks_to_the_bottom() {
    let Some(all) = candidates_for(3, "dddd") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("大厦"),
        "首选让给词，实际候选: {head:?}"
    );
    assert_eq!(
        all.iter().position(|t| t == "大"),
        Some(all.len() - 1),
        "有简码的字须沉到本码所有候选之后，实际候选: {head:?}"
    );
}

/// 沉底前的对照：档位 0 时「大」是首选、且列表里排在其余候选之前。
#[test]
fn disabled_keeps_the_char_on_top_for_a_multi_candidate_code() {
    let Some(all) = candidates_for(0, "dddd") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("大"),
        "档位 0 须完全按词库原序，实际候选: {head:?}"
    );
}

/// 反向对照：同一份词库，只把档位关到 0 → 首选回到词库原序的「你」。
///
/// 这一条证明主用例的「仰泳」是让位的结果，而不是词库本来就把它排在前面。
#[test]
fn disabled_keeps_dictionary_order() {
    let Some(all) = candidates_for(0, "wqiy") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("你"),
        "档位 0 须完全按词库原序（否则主用例是假绿），实际候选: {head:?}"
    );
}

/// 档位边界：`wq` 是二级简码，故档位 2 就该让位。
#[test]
fn second_level_shortcode_yields_at_level_two() {
    let Some(all) = candidates_for(2, "wqiy") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("仰泳"),
        "二简字在档位 2 下应当让位，实际候选: {head:?}"
    );
}

/// 简码位自身不让位：`wq` 是二简位，打到这里首选必须还是「你」。
///
/// 判据是「当前码长 > 档位」，若写成 `>=` 则简码位自己也会让位——用户连二简都打不出字了。
#[test]
fn shortcode_position_itself_keeps_the_char() {
    let Some(all) = candidates_for(3, "wq") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("你"),
        "简码位是让位的**来源**而不是对象，实际候选: {head:?}"
    );
}

/// 缺记录不让位：不逐键走到全码（这里直接打全码之外的路径无法构造，故用
/// 「首级记录被改码淘汰」等价场景）——`wqiy` 与 `wqiy` 之外的码不共享记录。
///
/// 与主用例的差别只有输入路径，锁住「判据来自沿途记录」这个设计；与
/// `disabled_keeps_dictionary_order` 合看能暴露记录没被消费的情形。
#[test]
fn char_without_shortcode_top_does_not_yield() {
    // 「匹」在 aq* 各级都不是首选（aq→区、aqt→获），故 aqtd 不因它而让位；
    // 该码首选本就是词，用于确认不会把非让位场景误判成让位。
    let Some(all) = candidates_for(3, "aqtd") else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("匹敌"),
        "首选本就是词时不应有任何改动，实际候选: {head:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 候选调整优先于出简让全
//
// `ShadowPin.position` 是用户所见（让位之后）列表里的绝对下标；让位若继续在 `apply_shadow`
// 之后动手，回放时会被挪走。故让位对「排过序的码」整码停手。
// ─────────────────────────────────────────────────────────────────────────────

/// 建一个独立的空 store（每个用例一个文件，避免相互污染）。
fn fresh_store(name: &str) -> (Arc<wind_store::Store>, PathBuf) {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    (store, path)
}

/// 同 [`candidates_for`]，但带 store —— shadow 规则存在 store 里，`new_headless` 的 store
/// 是 `None`，用它写候选调整相关的断言等于什么也没测（本仓栽过的形态）。
fn candidates_with_store(
    tag: &str,
    level: usize,
    code: &str,
    store: Arc<wind_store::Store>,
) -> Option<Vec<String>> {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：五笔词库不存在");
        return None;
    }
    let ov = override_with_level(tag, level);
    let coord =
        Coordinator::new_headless_with_store_override(wubi_config(), Some(&d), store, Some(ov));
    press(&coord, code);
    Some(coord.debug_all_candidate_texts())
}

/// 主用例：用户把「你」置顶回 `wqiy` 的首位 ⇒ 让位不得再把它沉底。
///
/// 这正是用户报的现场：开了出简让全之后，右键「置顶」写得进去、下次打同一个码却毫无变化。
#[test]
fn pinned_char_keeps_the_full_code_top() {
    let (store, path) = fresh_store("wind_scy_shadow_pinned.redb");
    store
        .pin_shadow("wubi86", "wqiy", "你", None, 0)
        .expect("pin_shadow 失败");
    let Some(all) = candidates_with_store("shadow_pinned", 3, "wqiy", store) else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("你"),
        "候选调整优先于出简让全，实际候选: {head:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 反向对照：同样带 store，一条规则都不写 ⇒ 让位照常发生。
///
/// 缺了这条，「凡是带 store 就不让位」的实现会让置顶用例假绿。
#[test]
fn store_without_pin_still_yields() {
    let (store, path) = fresh_store("wind_scy_shadow_nopin.redb");
    let Some(all) = candidates_with_store("shadow_nopin", 3, "wqiy", store) else {
        return;
    };
    let head: Vec<&str> = all.iter().take(6).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("仰泳"),
        "没有任何调整规则时须照常让位（否则主用例是假绿），实际候选: {head:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 停的是整码，不只是被沉底的那个字：这里置顶的是「硕大」，让位要沉底的是「大」。
///
/// 只赦免首条的实现会在这里红：让位的 rotate 会让接位词之后的候选各前移一位，
/// 用户「调到第 N 位」变成第 N-1 位。
#[test]
fn pinning_another_candidate_stops_the_yield_for_the_whole_code() {
    let (store, path) = fresh_store("wind_scy_shadow_other.redb");
    store
        .pin_shadow("wubi86", "dddd", "硕大", None, 1)
        .expect("pin_shadow 失败");
    let Some(all) = candidates_with_store("shadow_other", 3, "dddd", store) else {
        return;
    };
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("大"),
        "码上有任何置顶规则就位 ⇒ 整码停手，实际候选: {head:?}"
    );
    assert_eq!(
        all.get(1).map(|s| s.as_str()),
        Some("硕大"),
        "置顶规则本身须照常生效（否则上一条可能是 pin 压根没命中造成的），实际候选: {head:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 隐藏不算排过序：`deleted` 说的是「这条别出现」，与「谁排第一」不是同一维度。
///
/// 把 `deleted` 算进判据会让随手隐藏一条就静默关掉整码让位。无规则版本见
/// `the_yielding_char_sinks_to_the_bottom`。
#[test]
fn hiding_a_candidate_is_not_a_reorder() {
    let (store, path) = fresh_store("wind_scy_shadow_deleted.redb");
    store
        .delete_shadow("wubi86", "dddd", "硕大")
        .expect("delete_shadow 失败");
    let Some(all) = candidates_with_store("shadow_deleted", 3, "dddd", store) else {
        return;
    };
    let head: Vec<&str> = all.iter().take(8).map(|s| s.as_str()).collect();
    assert!(
        !all.iter().any(|t| t == "硕大"),
        "隐藏规则本身须生效（否则本用例测的不是删除路径），实际候选: {head:?}"
    );
    assert_eq!(
        all.first().map(|s| s.as_str()),
        Some("大厦"),
        "隐藏不是排序主张，让位须照常发生，实际候选: {head:?}"
    );
    assert_eq!(
        all.iter().position(|t| t == "大"),
        Some(all.len() - 1),
        "让位的字仍须沉到本码所有候选之后，实际候选: {head:?}"
    );
    let _ = std::fs::remove_file(&path);
}
