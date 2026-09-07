//! 临时拼音的候选调整（置顶 / 隐藏）—— 与拼音方案本身共享同一份规则。
//!
//! 用户报障：在全拼方案下把某个候选置顶，切回五笔用 `` ` `` 引导临拼打同样的音，
//! 置顶毫无效果。
//!
//! **根因不是归属桶取错，而是读端整段缺席**：`update_temp_pinyin_candidates` 的加工链
//! （排序 → 截断 → finalize → mark_common → apply_filter）里根本没有 `apply_shadow_in`
//! 这一步，规则写得进去、临拼永远读不出来。临拼是主输入路的平行实现，主路径每加一道
//! 加工都得两边各接一次，漏接完全静默——候选照出、顺序照排，只是少了一层重排。
//!
//! 桶本身一直是对齐的：`EngineManager::data_schema_id` 把所有 pinyin 型方案折叠到
//! `"pinyin"`，全拼 / 双拼 / 临拼目标方案落的是同一个桶。所以修法只是把读端接上，
//! 归属取**临拼目标方案**（不是 active——那是五笔）。
//!
//! ## ⚠️ 五条用例必须合看，缺一即可能假绿
//!
//! 前四条锁**归属轴**（规则算哪个方案的），第五条锁**码域轴**（规则的键长什么样）。
//! 两轴正交：任一轴的变异都不会让另一轴的用例变红，所以两边各要有自己的防线。
//!
//! - [`pin_in_pinyin_bucket_takes_effect_in_temp_pinyin`]：主用例；
//! - [`store_without_rule_keeps_dictionary_order`]：**反向对照**，同样带 store、只是不写
//!   规则 ⇒ 顺序回到词库原序。缺了它，「凡是带 store 就变序」之类的实现会让主用例假绿；
//! - [`rule_in_active_schema_bucket_does_not_leak`]：**变异防线**，规则写进 active 方案
//!   （`wubi86`）桶 ⇒ 临拼不得生效。归属若写成 `apply_shadow`（`None` ⇒ active），
//!   主用例红、本用例也红，两个方向各被锁住一次；
//! - [`hidden_in_pinyin_bucket_is_hidden_in_temp_pinyin`]：隐藏走同一条读端，与置顶
//!   是 `apply_shadow` 的两个维度（`deleted` / `pinned`），只测一边测不出另一边；
//! - [`shuangpin_target_reads_rule_under_normalized_full_pinyin_code`]：**码域轴**。
//!   前四条的临拼目标都是全拼，而全拼下 `ConvertResult::shadow_code` 恒为空串 ⇒ 归一
//!   那一支在它们眼里是死代码，删掉照样全绿。这条把临拼目标指向双拼才测得到。
//!
//! ★ **主方案刻意取 `wubi86` 而非 `pinyin`**：active 若也是拼音方案，「按 active 归属」
//! 这种错误实现同样能通过全部断言，等于什么都没锁住。这是本仓的既有教训（见
//! `temp_english_freq.rs` 的同名论证）。
//!
//! ★ **置顶目标从基线列表里现取**（`baseline[2]`）而非写死某个字：断言因此不依赖词库
//! 里 `ni` 的具体内容，词库更新不会让本族变脆。
//!
//! ⚠️ 词库缺失时整族**静默跳过而计数照绿**，判据是耗时（真跑约 1s 量级 vs 跳过 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/pinyin.schema.toml").exists()
        && d.join("schemas/pinyin/cn_dicts/41448.dict.yaml").exists()
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
    }
}

fn press_letter(coord: &Coordinator, c: char) {
    coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
}

/// 五笔主方案（临拼只在码表/混输方案下可用）+ 可用拼音方案。
fn wubi_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

/// 同上，但把**临拼目标方案指向双拼**（`temp_pinyin_target` 取 `schema.primary_pinyin`，
/// 空串才回落全拼）。供归一码那条用例用。
fn shuangpin_target_config() -> Config {
    let mut cfg = wubi_config();
    cfg.schema.available = vec!["wubi86".into(), "shuangpin".into()];
    cfg.schema.primary_pinyin = "shuangpin".into();
    cfg
}

/// 每个用例一个独立空 store（同一文件并发写会撕裂）。
fn fresh_store(name: &str) -> (Arc<wind_store::Store>, PathBuf) {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    (store, path)
}

/// 进入临拼、输入拼音，返回全部候选文本。
///
/// ⚠️ **必须用 `new_headless_with_store`**：shadow 规则存在 store 里，`new_headless` 的
/// store 是 `None`，用它写候选调整的断言等于什么也没测（本仓栽过的形态）。
fn temp_pinyin_candidates(store: Arc<wind_store::Store>, input: &str) -> Vec<String> {
    temp_pinyin_candidates_with(wubi_config(), store, input)
}

/// 同上，但可指定配置（供双拼目标那条用例换 `primary_pinyin`）。
fn temp_pinyin_candidates_with(
    cfg: Config,
    store: Arc<wind_store::Store>,
    input: &str,
) -> Vec<String> {
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    coord.handle_key_event(&key_event(0xC0)); // 反引号进入临拼
    assert!(coord.debug_in_temp_pinyin(), "反引号应进入临时拼音");
    for c in input.chars() {
        press_letter(&coord, c);
    }
    coord.debug_all_candidate_texts()
}

/// 直接用全拼方案打字（非临拼），作为「表现一致」的对照基准。
fn plain_pinyin_candidates(store: Arc<wind_store::Store>, input: &str) -> Vec<String> {
    let mut cfg = wubi_config();
    cfg.schema.active = "pinyin".into();
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    for c in input.chars() {
        press_letter(&coord, c);
    }
    coord.debug_all_candidate_texts()
}

/// 无任何规则时临拼的候选（取置顶目标用）。
fn baseline(tag: &str, input: &str) -> Option<Vec<String>> {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return None;
    }
    let (store, path) = fresh_store(&format!("wind_tps_base_{tag}.redb"));
    let all = temp_pinyin_candidates(store, input);
    let _ = std::fs::remove_file(&path);
    Some(all)
}

/// 主用例：全拼方案下置顶写入的规则（桶 = `"pinyin"`），临拼必须照样生效——
/// 「和原方案表现一致」就是这句话的断言形式，用户报障的正是它。
#[test]
fn pin_in_pinyin_bucket_takes_effect_in_temp_pinyin() {
    let Some(base) = baseline("pin", "ni") else {
        return;
    };
    assert!(
        base.len() >= 3,
        "前提：`ni` 应有足够候选，实际 {}",
        base.len()
    );
    let target = base[2].clone();
    assert_ne!(
        base[0], target,
        "前提：置顶目标须原本不在首位，否则断言无从区分"
    );

    let (store, path) = fresh_store("wind_tps_pin.redb");
    store
        .pin_shadow("pinyin", "ni", &target, None, 0)
        .expect("pin_shadow 失败");

    let temp = temp_pinyin_candidates(Arc::clone(&store), "ni");
    assert_eq!(
        temp.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "全拼桶里的置顶规则应在临拼生效（接上 apply_shadow_in 之前此处必红）。\n\
         临拼实际前 6 条: {:?}",
        &temp[..6.min(temp.len())]
    );

    // 「表现一致」的正面断言：同一条规则在**全拼方案**下也把它顶到首位 —— 两条路径读的
    // 是同一个桶、同一个码。
    //
    // ⚠️ 刻意**不断言两条列表逐项相同**。主路径在 `apply_filter` 之后还有临拼没有的加工：
    // 方案级短语合并、`apply_emoji_suggestions`、英文头部候选、空码补全收口、出简让全。
    // 今天 `ni` + 空 store + 默认档位下它们恰好都不动前 10 条，但那是**巧合不是不变量**
    // ——出厂短语哪天多一条码为 `ni` 的，逐项断言就会红，而消息会把人引到临拼上，
    // 真正的原因却在主路径多了一步临拼本就不该有的加工。
    let plain = plain_pinyin_candidates(Arc::clone(&store), "ni");
    assert_eq!(
        plain.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "同一条规则在全拼方案下也应把它顶到首位（本条红了先查主路径的加工链，别先怀疑临拼）。\n\
         全拼实际前 6 条: {:?}",
        &plain[..6.min(plain.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// **反向对照**：同样带 store，只是一条规则都不写 ⇒ 顺序回到词库原序。
///
/// 缺了这条，「凡是接上 store 就变序」「apply_shadow_in 无条件重排」之类的实现会让
/// 主用例假绿。它同时证明 `new_headless_with_store` 这条路径本身不改变候选顺序。
#[test]
fn store_without_rule_keeps_dictionary_order() {
    let Some(base) = baseline("nopin", "ni") else {
        return;
    };
    let (store, path) = fresh_store("wind_tps_nopin.redb");
    let temp = temp_pinyin_candidates(store, "ni");
    let n = 10.min(base.len()).min(temp.len());
    assert_eq!(
        &temp[..n],
        &base[..n],
        "没有任何规则时顺序不得变化（否则主用例是假绿）"
    );
    let _ = std::fs::remove_file(&path);
}

/// **变异防线**：把同一条规则写进 **active 方案**（`wubi86`）桶 ⇒ 临拼不得生效。
///
/// 归属若图省事写成 `self.apply_shadow(...)`（`schema_override = None` ⇒ 落 active），
/// 主用例会红、本用例也会红——两个失效方向各被锁住一次。只写主用例的话，一个「按
/// active 归属」的实现在主方案也是拼音时照样能通过，等于什么都没锁住。
#[test]
fn rule_in_active_schema_bucket_does_not_leak() {
    let Some(base) = baseline("leak", "ni") else {
        return;
    };
    assert!(base.len() >= 3, "前提：`ni` 应有足够候选");
    let target = base[2].clone();

    let (store, path) = fresh_store("wind_tps_leak.redb");
    store
        .pin_shadow("wubi86", "ni", &target, None, 0)
        .expect("pin_shadow 失败");

    let temp = temp_pinyin_candidates(store, "ni");
    assert_eq!(
        temp.first().map(|s| s.as_str()),
        base.first().map(|s| s.as_str()),
        "写在五笔桶里的规则不应影响临拼——临拼的候选是拼音方案出的，归属也该是它。\n\
         临拼实际前 6 条: {:?}",
        &temp[..6.min(temp.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 隐藏（`deleted`）与置顶（`pinned`）是 `apply_shadow` 的两个维度，走同一个读端。
/// 只测置顶的话，一个只搬运 `pinned` 的实现会让隐藏静默失效。
#[test]
fn hidden_in_pinyin_bucket_is_hidden_in_temp_pinyin() {
    let Some(base) = baseline("hide", "ni") else {
        return;
    };
    assert!(!base.is_empty(), "前提：`ni` 应有候选");
    let victim = base[0].clone();

    let (store, path) = fresh_store("wind_tps_hide.redb");
    store
        .delete_shadow("pinyin", "ni", &victim)
        .expect("delete_shadow 失败");

    let temp = temp_pinyin_candidates(store, "ni");
    assert!(
        !temp.contains(&victim),
        "全拼桶里隐藏掉的候选，临拼里也不该出现（实际前 6 条: {:?}）",
        &temp[..6.min(temp.len())]
    );
    assert!(!temp.is_empty(), "隐藏一条不应清空列表——那会是另一个缺陷");
    let _ = std::fs::remove_file(&path);
}

/// ★ **双拼归一码**：临拼目标方案是双拼时，规则键必须是**全拼码**。
///
/// 这一支是本次三处改动里唯一不被上面四条锁住的——全拼下 `ConvertResult::shadow_code`
/// **恒为空串**（`pinyin/mod.rs` 里只有双拼一臂产出它），于是把 `shadow_code` 的三元
/// 整段换回 `state.temp_pinyin_buffer.clone()`，前四条照样全绿。归属轴的变异
/// （`shadow_owner` → `None`）与码域轴正交，同样抓不到。
///
/// 出厂 `shuangpin` 是小鹤布局（`c` = ao）⇒ 击键 `hc` 对应全拼 `hao`。规则写在全拼码
/// `hao` 上（那正是双拼方案自己置顶时落的键），临拼打 `hc` 必须命中。不归一的话规则
/// 落 `hao`、读的是 `hc`，两个键互不相认，且完全静默——这正是 `shadow_code_of` 那套
/// 归一机制当初存在的理由，临拼此前把引擎给的归一码整个丢弃了。
#[test]
fn shuangpin_target_reads_rule_under_normalized_full_pinyin_code() {
    if !has_schemas() || !data_dir().join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    // 基线：无规则时双拼击键 `hc` 的候选。
    let (base_store, base_path) = fresh_store("wind_tps_sp_base.redb");
    let base = temp_pinyin_candidates_with(shuangpin_target_config(), base_store, "hc");
    let _ = std::fs::remove_file(&base_path);
    assert!(
        base.len() >= 3,
        "前提：双拼 `hc`(=hao) 应能出候选；为空说明目标方案没指到双拼或布局不是小鹤，\
         那样本用例测的就不是归一码了。实际: {base:?}"
    );
    let target = base[2].clone();
    assert_ne!(base[0], target, "前提：置顶目标须原本不在首位");

    // ★ 规则写在**全拼码** `hao` 上，不是击键 `hc`。
    let (store, path) = fresh_store("wind_tps_sp_pin.redb");
    store
        .pin_shadow("pinyin", "hao", &target, None, 0)
        .expect("pin_shadow 失败");

    let temp = temp_pinyin_candidates_with(shuangpin_target_config(), store, "hc");
    assert_eq!(
        temp.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "双拼临拼须按全拼归一码读规则（丢掉 result.shadow_code 时此处必红）。\n\
         临拼实际前 6 条: {:?}",
        &temp[..6.min(temp.len())]
    );
    let _ = std::fs::remove_file(&path);
}
