//! 出简让全的端到端验证（设计见 docs/design/codetable-short-code-yields-full.md）。
//!
//! 单元测试（`short_code_yield` 模块内）证明的是判定函数本身；本文件证明的是**用户入口
//! 上真的打得出来**——记录沿途累积、判据在真实候选链上成立、让位作用在显示序上。
//! 本仓的教训是这两层必须分开测：引擎/纯函数全绿而用户打不出，是反复出现过的形态。
//!
//! ## ⚠️ 档位统一走**方案级 override** 来设，别用全局配置
//!
//! 全局出厂是 0（关），且出厂方案**一律不声明**这一项（理由见 `data/config.toml` 与
//! `data/schemas/wubi86.schema.toml` 的同名注释）。方案级 `Some(_)` 恒覆盖全局，所以
//! 本文件统一走 `override_with_level` 而不是 `cfg.schema.codetable.short_code_yield_level`：
//! 后者眼下确实也能用，但只要哪天有方案声明了这一项，用它设的档位就会被静默覆盖，
//! 而**整组用例一起假绿**（期望让位的用例照样让位，只是原因换了个）。
//! `factory_wubi86_does_not_yield` 是唯一不压方案值的用例，它守的是「出厂即关」。
//!
//! ## ⚠️ 三条用例必须合看，缺一即可能假绿
//!
//! - `full_code_yields_to_word`：主用例，`wqiy` 首选从「你」变「仰泳」；
//! - `disabled_keeps_dictionary_order`：**反向对照**，同一份词库、只把档位关掉 → 首选回到
//!   「你」。它证明了词库原序确实是「你」在前，主用例不是因为词库本来就那样而假绿；
//! - `second_level_shortcode_yields_at_level_two`：档位边界，`wq` 是二简，故档位 2 也该让。
//!   若把档位判据写反（`>=` 写成 `>` 之类），这条会红而主用例照绿。
//!
//! ## ⚠️ 候选调整优先族：四条同样必须合看
//!
//! - `pinned_char_keeps_the_full_code_top`：用户把「你」置顶到 `wqiy` ⇒ 让位停手；
//! - `store_without_pin_still_yields`：**反向对照**，同样带 store、只是不写规则 ⇒ 仍让位。
//!   缺了它，「凡是带 store 就不让位」这类实现会让上一条假绿；
//! - `pinning_another_candidate_stops_the_yield_for_the_whole_code`：置顶的不是被沉底的
//!   那个字 ⇒ 整码照样停手。锁住的是「停整码」而非「只赦免首条」——让位的两步 rotate 会
//!   让接位词之后的候选各前移一位，只赦免首条治不了那一半；
//! - `hiding_a_candidate_is_not_a_reorder`：隐藏（`deleted`）**不算**排过序 ⇒ 仍让位。
//!   判据只数 `pinned`，缺了它，把删除也算进来的实现会静默关掉大量本该发生的让位。
//!
//! ⚠️ 这一族**必须用 `new_headless_with_store`**：shadow 规则存在 store 里，而
//! `new_headless` 的 store 是 `None` —— 用它写这些断言，测的东西压根不存在。
//!
//! 用例选 `wqiy`（你 / 仰泳）是历史原因：`khtk`（路 / 路程）当年在发行词库里已被
//! `gen_dict` 的 `[demotion]` 让过位，首选本就是词，测不出算法层有没有干活。
//! `[demotion]` 退役且词库重新生成后这条限制已消失（现产物 `khtk` 是「路」2383 >
//! 「路程」1435，与极点上游序一致），两个码现在都是可用现场。
//!
//! 词典缺失时自动跳过 —— ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**，
//! 判据是耗时（正常 1s 量级 vs 跳过 0.0x s）。

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

/// 逐键按下——**必须逐键**：让位的判据来自沿途各级简码位的首选记录，
/// 直接把缓冲设成全码的写法会让记录全空，于是恒不让位。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 全局配置**不设档位**——出厂全局是 0（关），各用例的档位由方案级 override 给出。
fn wubi_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 把档位写进**方案级 override**（`schema_overrides/wubi86.toml`），而不是全局配置。
///
/// ⚠️ 别退回老写法 `cfg.schema.codetable.short_code_yield_level = level`。出厂方案眼下
/// 不声明这一项，那样写**暂时**也跑得通——正因为如此它才危险：方案级 `Some(_)` 恒覆盖
/// 全局，哪天哪个方案声明了它，档位 0 的两条反向对照会变红，而档位 2 的边界用例会
/// **假绿**（它期望让位，方案里的值也让位）。override 层深合并在方案文件之后，是唯一
/// 压得住方案值的落点。
///
/// 每个用例一个目录（内容虽同，但并发写同一文件会撕裂），返回值直接喂
/// `new_headless_with_override`。
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

/// 出厂守门：**什么都不配**（全局出厂 0 + 无 override）时，wubi86 不让位。
///
/// 这是本功能唯一不经 override 的用例。上面所有用例都用 override 压掉了方案值，
/// 若只有它们，给 `data/schemas/wubi86.schema.toml` 补回一行
/// `short_code_yield_level = 3` 不会有任何测试变红——而每个五笔用户的候选顺序都变了，
/// 且他在全局页把这一项关掉也没用。config 侧的对应守门是
/// `wubi86_schema_does_not_declare_short_code_yield`（不需要词库，CI 上跑得到）。
///
/// 它顺带钉住了词库原序「你」在前，这正是 `disabled_keeps_dictionary_order`
/// 那条反向对照能成立的前提。
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

/// 让位的字沉到**本码所有候选之后**，不是降一位。
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

/// **反向对照**：同一份词库，只把档位关到 0 → 首选回到词库原序的「你」。
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
/// 与主用例的差别只有输入路径，用于锁住「判据来自沿途记录」这个设计本身：
/// 若哪天改成查询式实现，本用例仍绿而主用例也绿，但 `disabled_keeps_dictionary_order`
/// 与本用例的组合能暴露记录没被消费的情形。
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
// `ShadowPin.position` 是**绝对下标**，记的是用户右键当时所见列表里的位次；用户所见正是
// 让位之后的列表。让位若继续在 `apply_shadow` 之后动手，用户按下标 N 存进去、回放时被挪
// 到别处，那个下标就再也表达不了任何东西。故让位对「排过序的码」整码停手。
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

/// **反向对照**：同样带 store，只是一条规则都不写 ⇒ 让位照常发生。
///
/// 缺了这条，「凡是带 store 就不让位」「store 一接上让位链就断了」之类的实现会让主用例
/// 假绿。它同时证明 `new_headless_with_store` 这条路径本身没有改变让位行为。
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

/// 停的是**整码**，不只是被沉底的那个字：这里置顶的是「硕大」，而让位要沉底的是「大」。
///
/// 只赦免首条的实现会在这里红——让位仍会发生，首选变成「大厦」。而让位的两步 rotate 会把
/// 接位词之后的候选各前移一位，用户「调到第 N 位」照样会变成第 N-1 位，那半个失效正是本
/// 用例守的东西。
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

/// 隐藏**不算**排过序：`deleted` 说的是「这条别出现」，与「谁排第一」不是同一维度。
///
/// 把 `deleted` 也算进判据的实现会在这里红——用户随手隐藏一条候选，整码的让位就被静默
/// 关掉了。对照 `the_yielding_char_sinks_to_the_bottom`：那条是同一个码的无规则版本。
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
