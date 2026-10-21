//! `schema_dicts_api`（方案附加词库的列表与开关，桌面设置页与移动端门面共用）的行为契约。
//!
//! 桌面 RPC 形状与「saveConfig 快照不得冲掉开关」在 wind-webdata 的测试里验；这里验下沉后的
//! 本体：列表口径（不含主库、按方案文件顺序、生效开关）、翻转后重读与**重建协调器**后仍是
//! 新值、未知方案 / 词库报错，以及词库文件缺失时打开开关不崩、主库照常出词。
//!
//! 全部要 `build_dev/data`，缺失时打印跳过。override 目录与 redb 都落临时目录，不碰本机配置。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::schema_dicts_api::{self as api, SchemaDictEntry};
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir().join("schemas/pinyin.schema.toml").exists()
        && data_dir().join("schemas/wubi86.schema.toml").exists()
}

/// 临时根（redb + override 目录）。⚠️ 名字带 pid：多会话并行跑测试时固定名会互删夹具。
fn tmp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("wind_sdapi_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("overrides")).unwrap();
    root
}

/// 以 `root` 下的 redb 与 override 目录建协调器。同一个 `root` 再调一次 = 「重启」。
fn coord_at(root: &Path, data: &Path, active: &str) -> Arc<Coordinator> {
    let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = active.into();
    cfg.input.default.chinese_mode = true;
    Coordinator::new_headless_with_store_override(
        cfg,
        Some(data),
        store,
        Some(root.join("overrides")),
    )
}

fn find<'a>(list: &'a [SchemaDictEntry], id: &str) -> Option<&'a SchemaDictEntry> {
    list.iter().find(|d| d.id == id)
}

/// 拼音：列出随包细胞库、不含主库、按方案文件顺序；出厂全关。五笔：default_enabled 照实反映。
#[test]
fn lists_extra_dicts_in_file_order_without_main() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = tmp_root("list");
    let c = coord_at(&root, &data_dir(), "pinyin");

    let py = api::list(&*c, "pinyin").unwrap();
    assert!(find(&py, "pinyin_main").is_none(), "主库关不掉，不列");
    assert_eq!(py[0].id, "cell_exthot", "按方案文件顺序，首张是网络热词");
    let idiom = find(&py, "cell_idiom").expect("含成语库");
    assert_eq!(
        (
            idiom.label.as_str(),
            idiom.description.as_str(),
            idiom.enabled
        ),
        ("成语", "成语、俗语与古诗文名句", false),
        "细胞库出厂不勾选"
    );
    assert!(py.iter().all(|d| !d.enabled), "拼音附加库出厂全关: {py:?}");

    let wb = api::list(&*c, "wubi86").unwrap();
    let states: Vec<(&str, bool)> = wb.iter().map(|d| (d.id.as_str(), d.enabled)).collect();
    assert_eq!(
        states,
        [
            ("wubi86_extra", true),
            ("wubi86_emoji", true),
            ("wubi86_xzqy", false),
        ],
        "没翻过的取方案 default_enabled"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 开 / 关后再列状态翻转；override 只落稀疏项；「重启」（同目录重建协调器）后仍是新值。
#[test]
fn toggle_flips_listing_and_persists_across_rebuild() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = tmp_root("toggle");
    let enabled_of = |c: &Coordinator, schema: &str, id: &str| {
        find(&api::list(c, schema).unwrap(), id).unwrap().enabled
    };

    let c = coord_at(&root, &data_dir(), "pinyin");
    assert!(api::set_enabled(&*c, "pinyin", "cell_idiom", true).unwrap());
    assert!(enabled_of(&c, "pinyin", "cell_idiom"), "开了再列即为开");
    assert!(!enabled_of(&c, "pinyin", "cell_place"), "别的库不受牵连");
    // 默认开的库关掉——方向与上面相反，验的是覆盖压过 default_enabled
    api::set_enabled(&*c, "wubi86", "wubi86_emoji", false).unwrap();
    assert!(!enabled_of(&c, "wubi86", "wubi86_emoji"));

    let saved = std::fs::read_to_string(root.join("overrides/pinyin.toml")).unwrap();
    assert!(
        saved.contains("cell_idiom") && !saved.contains("path"),
        "override 只许 {{id, enabled}} 稀疏项：{saved}"
    );
    drop(c);

    let c = coord_at(&root, &data_dir(), "pinyin");
    assert!(enabled_of(&c, "pinyin", "cell_idiom"), "重建后仍开");
    assert!(!enabled_of(&c, "wubi86", "wubi86_emoji"), "重建后仍关");

    api::set_enabled(&*c, "pinyin", "cell_idiom", false).unwrap();
    drop(c);
    let c = coord_at(&root, &data_dir(), "pinyin");
    assert!(!enabled_of(&c, "pinyin", "cell_idiom"), "关回去也持久化");
    let _ = std::fs::remove_dir_all(&root);
}

/// 未知方案 / 方案里没有的库 → Err，且不落任何 override。
#[test]
fn unknown_schema_or_dict_is_error() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = tmp_root("unknown");
    let c = coord_at(&root, &data_dir(), "pinyin");

    let e = api::set_enabled(&*c, "pinyin", "no_such_dict", true).unwrap_err();
    assert!(e.to_string().contains("no_such_dict"), "报错要点名: {e}");
    assert!(api::set_enabled(&*c, "no_such_schema", "cell_idiom", true).is_err());
    assert!(api::list(&*c, "no_such_schema").is_err());
    assert!(
        !root.join("overrides/no_such_schema.toml").exists(),
        "未知方案不得落 override"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 打一串码、取前 20 个候选、Esc 清掉。
fn type_code(c: &Coordinator, code: &str) -> Vec<String> {
    let key = |vk: u32| {
        c.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        });
    };
    for ch in code.chars() {
        key(ch.to_ascii_uppercase() as u32);
    }
    let items = c.candidate_window(0, 20).items;
    key(0x1B);
    items
}

/// 开关对**已加载**的引擎即时生效（不重建协调器、不发重载）：成语库里才有的句子，
/// 开前打不出、开后打得出、关后又没了。
#[test]
fn toggle_takes_effect_on_loaded_engine() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = tmp_root("live");
    let c = coord_at(&root, &data_dir(), "pinyin");
    let has = |c: &Coordinator| {
        type_code(c, "aiyuanqisaoren")
            .iter()
            .any(|t| t == "哀怨起骚人")
    };

    assert!(!has(&c), "前提：主库里没有这句");
    api::set_enabled(&*c, "pinyin", "cell_idiom", true).unwrap();
    assert!(has(&c), "开了即出");
    api::set_enabled(&*c, "pinyin", "cell_idiom", false).unwrap();
    assert!(!has(&c), "关了即无");
    let _ = std::fs::remove_dir_all(&root);
}

/// 出厂包可以先不带 `pinyin/ext/*.dict.yaml`（Android APK 目前就是如此）：开关照常列、照常
/// 落盘，重建时那一层只记 warning，主库照常出词、不 panic。
///
/// 夹具：把 build_dev/data 逐项软链进临时目录，唯独 `schemas/pinyin/ext` 不链。
#[cfg(unix)]
#[test]
fn enabling_dict_with_missing_file_keeps_main_dict_working() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let root = tmp_root("missing");
    let data = root.join("data");
    let link_children = |from: &Path, to: &Path, skip: &str| {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_name() != skip {
                std::os::unix::fs::symlink(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    };
    let src = data_dir().canonicalize().unwrap();
    link_children(&src, &data, "schemas");
    link_children(&src.join("schemas"), &data.join("schemas"), "pinyin");
    link_children(
        &src.join("schemas/pinyin"),
        &data.join("schemas/pinyin"),
        "ext",
    );
    assert!(!data.join("schemas/pinyin/ext/idiom.dict.yaml").exists());

    let c = coord_at(&root, &data, "pinyin");
    // 先建一次引擎，开关走的就是「已加载 → 失效重建」那条路
    assert!(
        type_code(&c, "nihao").iter().any(|t| t == "你好"),
        "前提：主库出词"
    );
    assert!(
        find(&api::list(&*c, "pinyin").unwrap(), "cell_idiom").is_some(),
        "文件缺失不影响列出（登记在方案文件里）"
    );
    api::set_enabled(&*c, "pinyin", "cell_idiom", true).unwrap();
    assert!(
        find(&api::list(&*c, "pinyin").unwrap(), "cell_idiom")
            .unwrap()
            .enabled
    );

    let after = type_code(&c, "nihao");
    assert!(
        after.iter().any(|t| t == "你好"),
        "扩展层缺文件只少一层，主库照常出词，实得 {after:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
