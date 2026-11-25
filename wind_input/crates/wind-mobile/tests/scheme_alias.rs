//! 旧类型级键（`schema.pinyin.*` 等）在移动端门面上按别名读写：写到当前方案，快照读回方案的值。
//!
//! 单独成文件：`MobileCore::new` 会写进程环境变量，同进程并行跑多个会互相干扰。

use std::path::PathBuf;
use std::sync::Arc;

use wind_mobile::config_api::ConfigValue;
use wind_mobile::{CandidateFrame, InputStatus, MobileCore, MobileEventSink};

struct NullSink;

impl MobileEventSink for NullSink {
    fn on_candidates(&self, _: CandidateFrame) {}
    fn on_hide_candidates(&self) {}
    fn on_status(&self, _: InputStatus) {}
    fn on_toast(&self, _: String) {}
}

fn snapshot_value(core: &MobileCore, path: &[&str]) -> Option<toml::Value> {
    let root: toml::Table = core.config_snapshot().parse().ok()?;
    let mut cur = root.get(path[0])?;
    for p in &path[1..] {
        cur = cur.get(*p)?;
    }
    Some(cur.clone())
}

#[test]
fn alias_keys_write_to_and_read_from_the_active_scheme() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data_dir.join("config.toml").exists() {
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：缺少出厂数据 {}（CI 环境）", data_dir.display());
            return;
        }
        panic!(
            "缺少出厂数据 {}，本地不允许跳过（设 CI 环境变量可跳过）",
            data_dir.display()
        );
    }
    let user_root = std::env::temp_dir().join(format!("wind-mobile-alias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user_root);
    std::fs::create_dir_all(&user_root).unwrap();
    let core = MobileCore::new(
        &data_dir,
        &user_root,
        &user_root.join(".local"),
        "pinyin",
        Arc::new(NullSink),
    );

    let path = ["schema", "pinyin", "sentence_count"];
    let before = snapshot_value(&core, &path).and_then(|v| v.as_integer());
    let want = if before == Some(3) { 4 } else { 3 };
    core.config_set("schema.pinyin.sentence_count", ConfigValue::Int(want))
        .unwrap();

    assert_eq!(
        snapshot_value(&core, &path).and_then(|v| v.as_integer()),
        Some(want),
        "快照读回当前方案的值"
    );
    let user_cfg = std::fs::read_to_string(user_root.join("config.toml")).unwrap_or_default();
    assert!(
        !user_cfg.contains("sentence_count"),
        "不落进用户 config.toml：\n{user_cfg}"
    );

    let _ = std::fs::remove_dir_all(&user_root);
}
