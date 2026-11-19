//! 配对插入提交方式（`input.auto_pair.commit_via_composition`）的解析与默认值。
//!
//! 背景：AutoCAD 表格在位编辑器、Chromium 系宿主丢弃 `ITfInsertAtSelection` 直写
//! 却谎报 S_OK（2026-11-18 靶机实测），配对插入需可切到组合语义提交。默认关
//! （直写 = 长期现状）。compat 层 `pair_via_comp` 的三态解析见
//! `app_compat.rs` 单元测试 `pair_via_comp_three_states`。

use wind_config::Config;

#[test]
fn ships_disabled_write_through_is_the_default() {
    let cfg = Config::default();
    assert!(
        !cfg.input.auto_pair.commit_via_composition,
        "出厂必须关：直写是长期现状，全局翻转要等实测矩阵（IMPLEMENTATION_PLAN Stage 3）"
    );
}

#[test]
fn missing_key_in_old_toml_defaults_to_disabled() {
    // 老配置没有这个键 ⇒ serde 默认 false，不能让缺键变成开启（组合语义是行为变更）。
    let cfg: Config = toml::from_str("[input.auto_pair]\nenglish = true\n").unwrap();
    assert!(!cfg.input.auto_pair.commit_via_composition);
}

#[test]
fn explicit_true_enables() {
    let cfg: Config = toml::from_str("[input.auto_pair]\ncommit_via_composition = true\n").unwrap();
    assert!(cfg.input.auto_pair.commit_via_composition);
}
