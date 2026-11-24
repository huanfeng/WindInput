//! 应用级 `syllable_separator`：断言落在按键出口（组合串），不是判据返回值。
use super::*;
use wind_config::app_compat::{AppCompat, AppCompatRule, SyllableSeparator};

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn composition_after_typing(c: &Coordinator) -> String {
    let mut last = KeyAction::Consumed;
    for ch in "xiaopeng".chars() {
        last = c.handle_key_event_policed(&KeyEventData {
            key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
    match last {
        KeyAction::UpdateComposition { text, .. } => text,
        other => panic!("期望组合区更新，实得 {other:?}"),
    }
}

fn focus(c: &Coordinator, pid: u32, name: &str) {
    c.pid_names.lock().unwrap().insert(pid, name.into());
    c.active_compat.lock().unwrap().pid = pid;
    c.refresh_active_compat_lookups();
}

#[test]
fn app_rule_overrides_global_separator() {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        eprintln!("skip: 无 build_dev/data");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    let c = Coordinator::new_headless(cfg, Some(&data_dir()));
    c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
        process: "ue.exe".into(),
        syllable_separator: Some(SyllableSeparator::None),
        ..Default::default()
    }]));

    focus(&c, 7, "ue.exe");
    assert_eq!(
        composition_after_typing(&c),
        "xiaopeng",
        "配了 none 的应用无分隔符"
    );

    c.handle_key_event_policed(&KeyEventData {
        key_code: 0x1B,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
    focus(&c, 8, "other.exe");
    assert!(
        composition_after_typing(&c).contains('\''),
        "未配规则的应用跟随全局默认 '"
    );
}
