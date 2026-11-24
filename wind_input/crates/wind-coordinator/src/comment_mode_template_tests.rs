use super::*;

fn key(c: &Config, active: Option<ModeKind>) -> Option<String> {
    mode_key_of(c, active, |i| Some(format!("ov{i}")))
}

#[test]
fn fixed_modes_map_to_their_keys() {
    let c = Config::default();
    for (active, want) in [
        (ModeKind::TempPinyin, "temp_pinyin"),
        (ModeKind::TempEnglish, "temp_english"),
        (ModeKind::Url, "url"),
        (ModeKind::Email, "email"),
        (ModeKind::Unicode, "unicode"),
    ] {
        assert_eq!(key(&c, Some(active)).as_deref(), Some(want), "{active:?}");
    }
}

/// 生僻字、反查、辅助码与「无模式」都没有模式层。
#[test]
fn modes_without_layer() {
    let c = Config::default();
    for active in [
        None,
        Some(ModeKind::RareChar),
        Some(ModeKind::Reverse),
        Some(ModeKind::AuxCode),
    ] {
        assert_eq!(key(&c, active), None, "{active:?}");
    }
}

/// mix 实例按 id 取键，不按下标；越界（热重载删掉了该实例）没有模式层。
#[test]
fn mix_mode_key_uses_instance_id() {
    let c = Config::default();
    assert_eq!(c.schema.mix_modes[0].id, "quick_mix", "前置条件");
    assert_eq!(
        key(&c, Some(ModeKind::Mix(0))).as_deref(),
        Some("mix:quick_mix")
    );
    assert_eq!(key(&c, Some(ModeKind::Mix(7))), None);
}

/// 特殊模式的键是其方案 id；查不到方案时没有模式层，不 panic。
#[test]
fn special_mode_key_uses_schema_id() {
    let c = Config::default();
    assert_eq!(
        key(&c, Some(ModeKind::Special(3))).as_deref(),
        Some("special:ov3")
    );
    assert_eq!(mode_key_of(&c, Some(ModeKind::Special(3)), |_| None), None);
}
