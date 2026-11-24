//! 临时拼音的用户数据（候选调整 + 词频）——与拼音 / 双拼方案共享同一份。
//!
//! 纪律：临拼操作的是拼音那一份数据，不是主方案（五笔）的。两者编码域与码位结构不同
//! （拼音键是全拼扁平码、跨码位共享；码表键是输入码、码位独立且带按码长分级的简码保护），
//! 混进一个桶既读不出来，也让码表的保护策略去管拼音码位。读写四处（freq 读 / shadow 读 /
//! 两个上屏出口的写）一律取 `overlay_engine_schema`。`EngineManager::data_schema_id` 把所有
//! pinyin 型方案折叠到 `"pinyin"`，全拼 / 双拼 / 临拼目标方案落同一个桶。
//!
//! 临拼是主输入路的平行实现（`update_temp_pinyin_candidates`），主路径每加一道加工都得两边各接
//! 一次，漏接完全静默。
//!
//! ## 候选调整这五条必须合看（词频四条见文件后半）
//!
//! 前四条锁归属轴（规则算哪个方案的），第五条锁码域轴（规则的键长什么样），两轴正交。
//!
//! - [`pin_in_pinyin_bucket_takes_effect_in_temp_pinyin`]：主用例；
//! - [`store_without_rule_keeps_dictionary_order`]：反向对照，带 store 不写规则 ⇒ 词库原序；
//! - [`rule_in_active_schema_bucket_does_not_leak`]：规则写进 active（`wubi86`）桶 ⇒ 临拼不得生效；
//! - [`hidden_in_pinyin_bucket_is_hidden_in_temp_pinyin`]：隐藏与置顶是 shadow 读端的两个维度，
//!   只测一边测不出另一边；
//! - [`shuangpin_target_reads_rule_under_normalized_full_pinyin_code`]：码域轴，全拼下
//!   `shadow_code` 恒为空，只有临拼目标指向双拼才测得到归一。
//!
//! 主方案刻意取 `wubi86`：active 若也是拼音，「按 active 归属」的错误实现同样能通过
//! （见 `temp_english_freq.rs` 的同名论证）。置顶目标从基线列表现取（`baseline[2]`），不依赖词库内容。
//!
//! 词库缺失时整族静默跳过而计数照绿，判据是耗时（真跑约 1s 量级 vs 跳过 0.0x s）。

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
        seq_char: 0,
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

/// 同上，但把临拼目标方案指向双拼（`temp_pinyin_target` 取 `schema.primary_pinyin`，
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
/// 必须用 `new_headless_with_store`：shadow 规则存在 store 里，`new_headless` 的 store 是 `None`。
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

    // 正面断言：同一条规则在全拼方案下也把它顶到首位——两条路径读同一个桶、同一个码。
    // 刻意不断言两条列表逐项相同：主路径在 `apply_filter` 之后还有临拼没有的加工（方案级短语
    // 合并、emoji、英文头部候选、空码补全收口、出简让全），前 10 条一致只是巧合。
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

/// 反向对照：同样带 store、一条规则都不写 ⇒ 顺序回到词库原序。
///
/// 防「凡是接上 store 就变序」的实现让主用例假绿，同时证明 `new_headless_with_store` 本身不改顺序。
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

/// 变异防线：同一条规则写进 active 方案（`wubi86`）桶 ⇒ 临拼不得生效。
///
/// 归属若写成 `PassCtx { owner: None, .. }`（`None` ⇒ 落 active），主用例与本用例都会红。
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

/// 隐藏（`deleted`）与置顶（`pinned`）是 shadow 读端的两个维度，走同一个读端。
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

/// 双拼归一码：临拼目标方案是双拼时，规则键必须是全拼码。
///
/// 全拼下 `ConvertResult::shadow_code` 恒为空串（只有双拼一臂产出它），上面四条测不到这一支。
/// 出厂 `shuangpin` 是小鹤布局（`c` = ao）⇒ 击键 `hc` 对应全拼 `hao`；规则写在 `hao` 上
/// （双拼方案自己置顶时落的键），临拼打 `hc` 必须命中，不归一则两个键互不相认且完全静默。
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

    // 规则写在全拼码 `hao` 上，不是击键 `hc`。
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

// ─────────────────────────────────────────────────────────────────────────────
// 词频（`apply_freq_rerank_in` / `record_selection_in`）——与候选调整同一条归属纪律。
//
// 三处须一致取归属方案，且三者失效时互相掩盖：
//   ① 开关：取 active 会拿到码表那档（出厂 `enabled = false`），临拼一个字都不学；
//   ② 写端归属：键须落 `"pinyin"` 桶，不落 `"wubi86"`；
//   ③ 读端：`update_temp_pinyin_candidates` 须接 `apply_freq_rerank_in`。
// ─────────────────────────────────────────────────────────────────────────────

/// 进入临拼、打码、按空格选走首候选（触发记账）。返回被选中的文本。
fn temp_pinyin_commit_first(cfg: Config, store: Arc<wind_store::Store>, input: &str) -> String {
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
    coord.handle_key_event(&key_event(0xC0));
    for c in input.chars() {
        press_letter(&coord, c);
    }
    let first = coord
        .debug_all_candidate_texts()
        .first()
        .cloned()
        .unwrap_or_default();
    coord.handle_key_event(&key_event(0x20)); // 空格上屏首候选
    first
}

/// 写端归属（②）：临拼选词的词频落 `"pinyin"` 桶，不落主方案（`wubi86`）桶。
/// 归属退回 active（`record_selection`）时两条断言同时反向。
#[test]
fn temp_pinyin_freq_lands_in_pinyin_bucket_not_active_schema() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    // 必须显式打开：`Config::default()` 是结构体默认值、不读 `data/config.toml`，
    // 不设的话测的是一个关着的功能。开关本身另有两条用例。
    let mut cfg = wubi_config();
    cfg.schema.pinyin.frequency.enabled = true;
    let (store, path) = fresh_store("wind_tps_freq_bucket.redb");
    let picked = temp_pinyin_commit_first(cfg, Arc::clone(&store), "ni");
    assert!(!picked.is_empty(), "前提：临拼 `ni` 应有候选可选");

    // 记账码按候选来源分流：拼音取候选码（全拼扁平码），对 `ni` 的单字即 `ni`。
    let in_pinyin = store.get_freq("pinyin", "ni", &picked).unwrap();
    let in_wubi = store.get_freq("wubi86", "ni", &picked).unwrap();
    assert!(
        in_pinyin.is_some(),
        "临拼选词的词频应落 pinyin 桶（选中 {picked:?}）"
    );
    assert!(
        in_wubi.is_none(),
        "不得落主方案桶——拼音码与五笔码位的编码域和结构都不同（选中 {picked:?}）"
    );
    let _ = std::fs::remove_file(&path);
}

/// 开关跟着归属走（①）：出厂配置下（码表调频关、拼音调频开）临拼必须记词频。
///
/// `freq_settings_for` 按 engine_type 分流，取 active 会拿到码表那档。反向对照
/// [`temp_pinyin_freq_respects_pinyin_switch_off`] 证明读的是拼音那档而非「无条件记」。
/// `Config::default()` 不读 `data/config.toml`，两者调频默认值不一致，故两个开关一律显式设。
#[test]
fn temp_pinyin_freq_follows_pinyin_switch_not_codetable() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let mut cfg = wubi_config();
    cfg.schema.codetable.frequency.enabled = false; // 出厂：码表关
    cfg.schema.pinyin.frequency.enabled = true; // 出厂：拼音开
    let (store, path) = fresh_store("wind_tps_freq_switch_on.redb");
    let picked = temp_pinyin_commit_first(cfg, Arc::clone(&store), "ni");
    assert!(
        store.get_freq("pinyin", "ni", &picked).unwrap().is_some(),
        "出厂组合（码表关/拼音开）下临拼应照常记词频；\
         开关若取 active 的码表档，这里一个字都不会记（选中 {picked:?}）"
    );
    let _ = std::fs::remove_file(&path);
}

/// 反向对照：关掉拼音调频 ⇒ 临拼不记。防「无条件记账」的实现让上一条假绿。
#[test]
fn temp_pinyin_freq_respects_pinyin_switch_off() {
    if !has_schemas() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let mut cfg = wubi_config();
    cfg.schema.codetable.frequency.enabled = true; // 故意与拼音档相反
    cfg.schema.pinyin.frequency.enabled = false;
    let (store, path) = fresh_store("wind_tps_freq_switch_off.redb");
    let picked = temp_pinyin_commit_first(cfg, Arc::clone(&store), "ni");
    assert!(
        store.get_freq("pinyin", "ni", &picked).unwrap().is_none(),
        "拼音调频关闭时临拼不得记账（选中 {picked:?}）"
    );
    assert!(
        store.get_freq("wubi86", "ni", &picked).unwrap().is_none(),
        "更不得因为码表档开着就落进码表桶（选中 {picked:?}）"
    );
    let _ = std::fs::remove_file(&path);
}

/// 读端（③）：全拼方案里学到的词频，临拼里照样生效（两条路径共享 `"pinyin"` 桶）。
/// 直接往 pinyin 桶写一条足够重的记录，再看临拼是否把它提到首位，失败时指向更清楚。
#[test]
fn freq_learned_in_pinyin_schema_reranks_temp_pinyin() {
    let Some(base) = baseline("freq_read", "ni") else {
        return;
    };
    assert!(base.len() >= 3, "前提：`ni` 应有足够候选");
    let target = base[2].clone();
    assert_ne!(base[0], target, "前提：目标须原本不在首位");

    let (store, path) = fresh_store("wind_tps_freq_read.redb");
    // 记几次，确保 used-first 足以把它顶上来。
    for _ in 0..5 {
        store
            .record_freq("pinyin", "ni", &target)
            .expect("record_freq 失败");
    }

    let mut cfg = wubi_config();
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    coord.handle_key_event(&key_event(0xC0));
    for c in "ni".chars() {
        press_letter(&coord, c);
    }
    let temp = coord.debug_all_candidate_texts();
    assert_eq!(
        temp.first().map(|s| s.as_str()),
        Some(target.as_str()),
        "pinyin 桶里的词频应在临拼生效（读端缺席时此处必红）。\n临拼实际前 6 条: {:?}",
        &temp[..6.min(temp.len())]
    );
    let _ = std::fs::remove_file(&path);
}

/// 词频重排必须用归属方案的算法，不是活跃引擎的。
///
/// 五笔主方案下若按活跃引擎选算法，临拼的拼音候选会走进码表 used-first 分支（用过即跳到档内
/// 最前且不衰减），而拼音的位置提升模型是渐进前移；`freq_settings_for` 给拼音的
/// `strategy/protect` 只是占位值，前提正是拼音走不到码表分支。
///
/// 判据取渐进性：记一次后应前移但不到首位，码表分支会一步跳到最前。与
/// [`freq_learned_in_pinyin_schema_reranks_temp_pinyin`]（记 5 次后到首位）合看。
#[test]
fn temp_pinyin_freq_uses_pinyin_model_not_codetable_used_first() {
    let Some(base) = baseline("model", "ni") else {
        return;
    };
    // 取一个足够靠后的候选：位次减半后仍不该到首位。
    let idx = 8;
    if base.len() <= idx {
        eprintln!("跳过：`ni` 候选不足 {} 条", idx + 1);
        return;
    }
    let target = base[idx].clone();

    let (store, path) = fresh_store("wind_tps_freq_model.redb");
    store
        .record_freq("pinyin", "ni", &target)
        .expect("record_freq 失败");

    let mut cfg = wubi_config();
    cfg.schema.pinyin.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    coord.handle_key_event(&key_event(0xC0));
    for c in "ni".chars() {
        press_letter(&coord, c);
    }
    let after = coord.debug_all_candidate_texts();
    let pos = after.iter().position(|t| *t == target).unwrap_or_else(|| {
        panic!(
            "目标 {target:?} 不该消失，实际: {:?}",
            &after[..8.min(after.len())]
        )
    });

    assert!(
        pos < idx,
        "记过一次词频应当前移：{target:?} 原第 {idx} 位、现第 {pos} 位"
    );
    assert!(
        pos > 0,
        "记**一次**不该直接跳到首位——那是码表 used-first 的形状，说明算法分支问的是活跃引擎\
         而非归属方案（{target:?} 原第 {idx} 位）"
    );
    let _ = std::fs::remove_file(&path);
}
