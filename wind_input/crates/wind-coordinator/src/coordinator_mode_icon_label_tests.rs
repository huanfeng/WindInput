use crate::coordinator::Coordinator;
use std::sync::Arc;
use wind_config::Config;

fn coord_with(english: &str, caps: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.ui.labels.english = english.to_string();
    cfg.ui.labels.caps_lock = caps.to_string();
    Coordinator::new_headless(cfg, None)
}

/// 配了就用配的。
#[test]
fn configured_labels_take_effect() {
    let c = coord_with("En", "Cp");
    assert_eq!(c.mode_icon_label(false, false), "En", "英文半角");
    assert_eq!(c.mode_icon_label(false, true), "Cp", "大写锁定");
    assert_eq!(
        c.mode_icon_label(true, true),
        "Cp",
        "中文 + 大写锁定：大写锁定优先（对齐 effective_chinese）"
    );
}

/// 留空回落内置默认，不是回落空：空标签会让语言栏图标画出一个没有主字的空白方块。
#[test]
fn empty_falls_back_to_builtin() {
    let c = coord_with("", "");
    assert_eq!(c.mode_icon_label(false, false), "英");
    assert_eq!(c.mode_icon_label(false, true), "A");
}

/// 出厂配置必须与改造前的字面量一致：老用户升级后不该看到任何变化。
#[test]
fn default_config_preserves_legacy_labels() {
    let c = Coordinator::new_headless(Config::default(), None);
    assert_eq!(c.mode_icon_label(false, false), "英");
    assert_eq!(c.mode_icon_label(false, true), "A");
}

/// 反向对照：英文标签不得渗进中文态。缺了这条，「所有分支都返回 english_label」的错误实现能让
/// 上面的断言全绿。无方案数据时中文态回落「中」，不需要 fixture 就能钉死。
#[test]
fn english_label_does_not_leak_into_chinese() {
    let c = coord_with("En", "Cp");
    assert_eq!(
        c.mode_icon_label(true, false),
        "中",
        "中文态取方案侧 icon_label，与 [ui.labels] 无关"
    );
}

/// 超长配置在这一层就被截断。
///
/// 这是 C++ 侧 `_inputTypeLabel` 缓冲的第一道防线（第二道是那边的 `_TRUNCATE`），挂了意味着
/// 超长标签能一路走到 `wcsncpy_s`。
#[test]
fn overlong_label_truncated_before_leaving_coordinator() {
    let c = coord_with("English", "CapsLock");
    assert_eq!(c.mode_icon_label(false, false), "En");
    assert_eq!(c.mode_icon_label(false, true), "Ca");
}
