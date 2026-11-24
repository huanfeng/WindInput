//! 跨仓对账：邻仓 `wind-setting` 的三份检入产物不得与 core 漂移。
//!
//! 邻仓 wind-setting 的守护测试（如 `snapshot_matches_core_generated_capabilities`）依赖
//! `windui`，在 Linux 上编译不了；本文件把对账搬到改配置的 core 一侧，两个生成器的输入端
//! （`wind_rpc::capabilities::generate` / `wind_config::Config::system_preset_value`）都在这里。
//!
//! 不替代邻仓那三个守护测试：这里只对账 core 看得见的事实（键在不在、类型与默认值）。
//! 值的策展偏离（`CURATED_MOCK_VALUES`）、控件类型、选项文案仍由邻仓把关。
//!
//! 找不到 `../wind-setting` 时整族跳过，每条跳过都显式打印原因，且判据只有「目录不存在」
//! 这一种——文件缺失、JSON 解析失败一律当失败。邻仓位置可用 `WIND_SETTING_DIR` 覆盖。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// core 的数据目录（`data/`）——capability 的默认值取自它的 L2 预置。
fn core_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

/// 邻仓根目录；`None` = 本机没有它，调用方跳过。
fn setting_repo() -> Option<PathBuf> {
    let dir = match std::env::var_os("WIND_SETTING_DIR") {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../wind-setting"),
    };
    dir.is_dir().then_some(dir)
}

/// 跳过时统一打印，免得「没跑」和「跑过了」在输出里长得一样。
fn skip(what: &str) {
    eprintln!("跳过 {what}：本机没有 ../wind-setting（设 WIND_SETTING_DIR 可指定位置）");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读不了 {}: {e}", path.display()))
}

/// capability 文档按 `configKeys[].key` 建索引——顺序不参与对账（生成器的数组序会随注册表变，
/// 而漂移与否只看每个键的描述是否一致）。
fn index_config_keys(v: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    v.get("configKeys")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    i.get("key")
                        .and_then(|k| k.as_str())
                        .map(|k| (k.to_string(), i.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 检入的 `capabilities.snapshot.json` 必须等于 core 现场生成的 capability descriptor。
///
/// 只比 `configKeys`：`appVersion` 跟着 `docs/VERSION` 走，与配置契约无关。
///
/// 失败时去邻仓重新生成，别手改 JSON：
/// `cd ../wind-setting && cargo test regenerate_capabilities_snapshot -- --ignored`
/// （那条命令要 Windows/macOS，见 `wind-setting/src/capabilities.rs` 的说明）。
#[test]
fn capabilities_snapshot_matches_core() {
    let Some(repo) = setting_repo() else {
        return skip("capability 快照对账");
    };
    let fresh = wind_rpc::capabilities::generate(Some(&core_data_dir()))
        .expect("生成 core capability 失败");
    let snapshot: serde_json::Value =
        serde_json::from_str(&read(&repo.join("src/assets/capabilities.snapshot.json")))
            .expect("capabilities.snapshot.json 不是合法 JSON");

    let (core, snap) = (index_config_keys(&fresh), index_config_keys(&snapshot));
    assert!(!core.is_empty(), "core 生成结果为空，对账无意义");

    let mut problems = Vec::new();
    for (key, cv) in &core {
        match snap.get(key) {
            None => problems.push(format!("快照缺键: {key}")),
            Some(sv) if sv != cv => problems.push(format!("{key}: 快照={sv} core={cv}")),
            _ => {}
        }
    }
    for key in snap.keys().filter(|k| !core.contains_key(*k)) {
        problems.push(format!("快照多余键（core 已无此键）: {key}"));
    }
    assert!(
        problems.is_empty(),
        "capabilities.snapshot.json 与 core 漂移 {} 处：\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// `mockdata/config.json`（设置页离线演示用的配置）必须含有 core 预置里的每个键。
/// 只比键不比值：邻仓 `CURATED_MOCK_VALUES` 有意偏离预置值，比值必然误报；值的对账归邻仓。
#[test]
fn mock_config_has_every_preset_key() {
    let Some(repo) = setting_repo() else {
        return skip("mockdata 键集合对账");
    };
    let preset = wind_config::Config::system_preset_value(Some(&core_data_dir()))
        .expect("读取 core 系统预置失败");
    let cfg: wind_config::Config = preset.try_into().expect("系统预置反序列化为 Config 失败");
    let expect = serde_json::to_value(&cfg).expect("Config 序列化失败");
    let mock: serde_json::Value =
        serde_json::from_str(&read(&repo.join("src/mockdata/config.json")))
            .expect("mockdata/config.json 不是合法 JSON");

    // 注册表是配置键的单一真相源；用它而不是递归 JSON，免得把 map 型值的用户键
    // （`input.punct.custom_mappings` 里那些标点）当成配置键要求 mockdata 一一对齐。
    let missing: Vec<&str> = wind_config::config_schema::registry()
        .iter()
        .map(|f| f.key)
        .filter(|key| {
            let ptr = format!("/{}", key.replace('.', "/"));
            expect.pointer(&ptr).is_some() && mock.pointer(&ptr).is_none()
        })
        .collect();
    assert!(
        missing.is_empty(),
        "mockdata/config.json 缺 {} 个键：{missing:?}\n\
         去邻仓重新生成：cargo test regenerate_mock_config -- --ignored",
        missing.len()
    );
}

/// core 新增的配置键必须要么进设置清单、要么进豁免名单（no-silent-caps 的 core 侧半边）。
/// 只做单向：「core 有、清单没有、名单也没有」才报；反向的死条目留给邻仓的
/// `uncovered_capability_keys_match_allowlist`。
///
/// 豁免名单是从 `capabilities.rs` 的 `UNCOVERED_BY_DESIGN` 源码文本里抠的（邻仓常量 use 不到）。
/// 抠不到时跳过而不是当成空名单，否则合理豁免的键会全报成漂移。
#[test]
fn every_core_key_is_either_in_the_manifest_or_exempt() {
    let Some(repo) = setting_repo() else {
        return skip("设置清单覆盖对账");
    };
    let manifest: toml::Value =
        toml::from_str(&read(&repo.join("src/assets/settings_manifest.toml")))
            .expect("settings_manifest.toml 不是合法 TOML");
    let covered: BTreeSet<&str> = manifest
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|it| it.get("key").and_then(|k| k.as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !covered.is_empty(),
        "设置清单里一个 key 都没解析出来，对账无意义"
    );

    let Some(exempt) = parse_uncovered_by_design(&read(&repo.join("src/capabilities.rs"))) else {
        eprintln!(
            "跳过设置清单覆盖对账：没能从 capabilities.rs 抠出 UNCOVERED_BY_DESIGN（它改结构了？）"
        );
        return;
    };

    let unlisted: Vec<&str> = wind_config::config_schema::registry()
        .iter()
        .map(|f| f.key)
        .filter(|k| !covered.contains(*k) && !exempt.contains(*k))
        .collect();
    assert!(
        unlisted.is_empty(),
        "这 {} 个 core 配置键既没接进 settings_manifest.toml，也不在 UNCOVERED_BY_DESIGN：\n  {}\n\
         —— 要么给它加一项清单，要么登记进豁免名单并写明理由（两件事都在 ../wind-setting）",
        unlisted.len(),
        unlisted.join("\n  ")
    );
}

/// 从 `capabilities.rs` 源码里抠 `UNCOVERED_BY_DESIGN` 的字符串字面量。
///
/// 只取常量块内、剥掉 `//` 注释之后的 `"..."`：注释里有已撤出名单的键名，连注释一起抠会放水。
fn parse_uncovered_by_design(src: &str) -> Option<BTreeSet<String>> {
    let start = src.find("UNCOVERED_BY_DESIGN")?;
    let open = src[start..].find('[')? + start;
    let end = src[open..].find("];")? + open;
    let mut out = BTreeSet::new();
    for line in src[open..end].lines() {
        let code = line.split("//").next().unwrap_or("");
        let mut rest = code;
        while let Some(a) = rest.find('"') {
            let after = &rest[a + 1..];
            let Some(b) = after.find('"') else { break };
            out.insert(after[..b].to_string());
            rest = &after[b + 1..];
        }
    }
    (!out.is_empty()).then_some(out)
}
