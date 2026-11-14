//! 方案级自定义标点（`[punct] custom_mappings`）经设置页保存后，**吃键闸门**必须立即认账。
//!
//! # 报障（GH#144）
//!
//! 设置 → 方案 → 方案设置 →「自定义标点」配了之后不生效。出字侧（`effective_punct`）每次
//! 按键现查 `behavior_for`，`schema.saveConfig` 写 override 时顺手失效了该缓存，所以出字
//! 从来是对的——只看 `handle_key_event` 的探针全绿。坏的是**吃键**那一侧：
//! `cn_passthrough_punct_chars`（推给 DLL 的「中文模式该透传的标点」）在 `ConfigBundle`
//! 里算好后就定住了，而其中减去的「任一方案自定义表覆盖过的键」只在 bundle 重建时重算。
//! 设置页只改方案 override 时根本不调 `config.setItems`，bundle 不重建 ⇒ 中文标点表里没有
//! 映射的键（`/` `@` `#` `%` `&` `*` `-` `=` …）仍在透传集里，DLL 在 `OnTestKeyDown`
//! 直接放行给宿主，core 连按键都收不到。
//!
//! 与方案类型无关（码表 / 混输同病）；报障里「码表生效、混输不生效」取决于测的是哪个键、
//! 期间是否恰好改过别的全局设置或重启过服务。
//!
//! `should_handle_key` 读的正是推给 DLL 的同一份集合（见 `key_gate.rs` 第 6 段），故在此断言
//! 它即等于断言 DLL 侧的吃键判定。

use std::path::PathBuf;

use serde_json::json;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_host::KeyProbe;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_webdata::WebDataRpc;

const VK_OEM_2: u32 = 0xBF; // `/`：中文标点表无映射 ⇒ 出厂在透传集里

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready() -> bool {
    let ok = data_dir()
        .join("schemas/wubi86_pinyin.schema.toml")
        .exists();
    if !ok {
        eprintln!("跳过：缺少 build_dev/data 方案");
    }
    ok
}

fn ev(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 模拟设置页「方案设置 → 自定义标点」的保存：getConfig → 改表 → saveConfig。
/// 与 wind-setting `stage_settings_edits` / `commit_pending_edits` 走同一对 RPC，
/// 且**只**走这一对——那边只改方案 override 时不会调 `config.setItems`。
fn save_schema_punct(c: &Coordinator, id: &str) {
    let mut sc = c
        .web_data_rpc("schema.getConfig", &json!({ "id": id }))
        .unwrap();
    sc["punct"]["custom_mappings"] = json!({ "/": ["Y", "Y", "Y", "Y"] });
    c.web_data_rpc("schema.saveConfig", &json!({ "id": id, "cfg": sc }))
        .unwrap();
}

fn check(active: &str) {
    // 目录名带进程号：并发会话共用 TMPDIR 时不互相覆盖。
    let ov_dir = std::env::temp_dir().join(format!("wind_gh144_{active}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov_dir);
    std::fs::create_dir_all(&ov_dir).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into(), "wubi86_pinyin".into()];
    cfg.schema.active = active.into();
    cfg.input.default.chinese_mode = true;
    cfg.input.default.chinese_punct = true;
    let c = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov_dir.clone()));

    // 前提：出厂 `/` 在透传集里（否则本用例测不出东西，见 memory「护栏要先验可观测性」）。
    assert!(
        !c.should_handle_key(&KeyProbe::new(VK_OEM_2)),
        "[{active}] 前提不成立：出厂 `/` 本该透传"
    );

    save_schema_punct(&c, active);

    assert!(
        c.should_handle_key(&KeyProbe::new(VK_OEM_2)),
        "[{active}] 方案表配了 `/` 之后吃键闸门仍放行 ⇒ DLL 把 `/` 直接交给宿主，自定义值永远出不来"
    );
    // 出字侧本就对，一并钉住：吃下来之后确实出方案配的值。
    match c.handle_key_event(&ev(VK_OEM_2)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "Y", "[{active}]"),
        other => panic!("[{active}] 期望上屏 Y，实得 {other:?}"),
    }

    // 反向：清掉方案表后必须回到透传（否则 `/` 在非 TSF 宿主上会被吃了再吐）。
    let mut sc = c
        .web_data_rpc("schema.getConfig", &json!({ "id": active }))
        .unwrap();
    sc["punct"]["custom_mappings"] = serde_json::Value::Null;
    c.web_data_rpc("schema.saveConfig", &json!({ "id": active, "cfg": sc }))
        .unwrap();
    assert!(
        !c.should_handle_key(&KeyProbe::new(VK_OEM_2)),
        "[{active}] 方案表清空后 `/` 应回到透传"
    );

    let _ = std::fs::remove_dir_all(&ov_dir);
}

#[test]
fn codetable_schema_punct_save_refreshes_key_gate() {
    if ready() {
        check("wubi86");
    }
}

#[test]
fn mixed_schema_punct_save_refreshes_key_gate() {
    if ready() {
        check("wubi86_pinyin");
    }
}

/// CLI `schema set` 直接写 override 文件后调 `schema.invalidate`，与 saveConfig 同形：
/// 同样必须重建吃键集（GH#144 同根）。
#[test]
fn schema_invalidate_refreshes_key_gate() {
    if !ready() {
        return;
    }
    let ov_dir = std::env::temp_dir().join(format!("wind_gh144_inval_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov_dir);
    std::fs::create_dir_all(&ov_dir).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.default.chinese_punct = true;
    let c = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov_dir.clone()));
    assert!(
        !c.should_handle_key(&KeyProbe::new(VK_OEM_2)),
        "前提：出厂 `/` 本该透传"
    );

    std::fs::write(
        ov_dir.join("wubi86.toml"),
        "[punct.custom_mappings]\n\"/\" = [\"Y\", \"Y\", \"Y\", \"Y\"]\n",
    )
    .unwrap();
    c.web_data_rpc("schema.invalidate", &json!({ "id": "wubi86" }))
        .unwrap();
    assert!(
        c.should_handle_key(&KeyProbe::new(VK_OEM_2)),
        "CLI 写 override + schema.invalidate 后吃键闸门应认账"
    );
    let _ = std::fs::remove_dir_all(&ov_dir);
}
