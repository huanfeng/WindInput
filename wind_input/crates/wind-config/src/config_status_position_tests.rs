use super::*;
use crate::app_compat::{StatusAnchor, StatusFallback, StatusPositionMode};

/// 出厂默认 = 本功能引入前的行为：跟随光标、坐标不可信时用最近一次有效坐标。
#[test]
fn defaults_keep_previous_behavior() {
    let si = StatusIndicatorConfig::default();
    assert_eq!(si.position(), StatusPositionMode::FollowCaret);
    assert_eq!(si.fallback(), StatusFallback::Last);
}

#[test]
fn anchors_parse() {
    let si = StatusIndicatorConfig {
        position_mode: "window_center".into(),
        fallback_position: "screen_bottom_left".into(),
        ..Default::default()
    };
    assert_eq!(
        si.position(),
        StatusPositionMode::Anchor(StatusAnchor::WindowCenter)
    );
    assert_eq!(
        si.fallback(),
        StatusFallback::Anchor(StatusAnchor::ScreenBottomLeft)
    );
}

/// 手写配置拼错：加载照常成功，解析回落出厂默认，而不是报错或当成别的档。
#[test]
fn typos_fall_back_to_defaults() {
    let cfg: Config = toml::from_str(
        "[ui.status]\nposition_mode = \"screen_centre\"\nfallback_position = \"hidden\"\n",
    )
    .expect("拼错不得让配置加载失败");
    assert_eq!(cfg.ui.status.position(), StatusPositionMode::FollowCaret);
    assert_eq!(cfg.ui.status.fallback(), StatusFallback::Last);
}

#[test]
fn reverse_bound_action_parses() {
    assert_eq!(BoundAction::parse("reverse"), BoundAction::Reverse);
    assert_eq!(BoundAction::parse(" Reverse "), BoundAction::Reverse);
    assert_eq!(
        BoundAction::parse("reversex"),
        BoundAction::None,
        "未知动词仍回落 None"
    );
}

/// 整串反查字数上限：缺省 16，可被配置覆盖；其余字段缺省不受影响。
#[test]
fn reverse_clip_max_chars_defaults_to_16_and_parses() {
    assert_eq!(Config::default().input.reverse.clip_max_chars, 16);
    let t: ReverseConfig = toml::from_str("lookup_disabled_dicts = true\n").unwrap();
    assert_eq!(t.clip_max_chars, 16, "缺省段落也是 16，不是 usize 零值");
    let t: ReverseConfig = toml::from_str("clip_max_chars = 8\n").unwrap();
    assert_eq!(t.clip_max_chars, 8);
}

/// 含未启用扩展词库是反查模式专属的全局开关，出厂关。
#[test]
fn reverse_lookup_disabled_dicts_defaults_off_and_parses() {
    assert!(
        !Config::default().input.reverse.lookup_disabled_dicts,
        "出厂关"
    );
    let t: ReverseConfig = toml::from_str("lookup_disabled_dicts = true\n").unwrap();
    assert!(t.lookup_disabled_dicts);
    assert_eq!(
        t.candidate_layout,
        LayoutIntent::Follow,
        "缺省的布局不受影响"
    );
}

#[test]
fn reverse_config_defaults_off_and_parses() {
    let c = Config::default();
    assert_eq!(c.input.reverse.candidate_layout, LayoutIntent::Follow);
    // 旧总开关 `enabled` 已退役：残留在串里被忽略，不影响同段其它字段。
    let t: ReverseConfig =
        toml::from_str("enabled = true\ncandidate_layout = \"vertical\"\n").unwrap();
    assert_eq!(t.candidate_layout, LayoutIntent::Vertical);
    assert!(!t.lookup_disabled_dicts);
    let bad: ReverseConfig = toml::from_str("candidate_layout = \"diagonal\"\n").unwrap();
    assert_eq!(
        bad.candidate_layout,
        LayoutIntent::Follow,
        "非法值容错回落（tolerant_de）"
    );
}

/// spec §3.2：模式内通配键取方案键；键非法（含通配关时的出厂 "z"）一律回落 z。
#[test]
fn reverse_wildcard_char_falls_back_to_z() {
    let g = CodetableGlobal::default();
    assert_eq!(g.reverse_wildcard_char(), 'z');
    let q = CodetableGlobal {
        wildcard_key: "?".into(),
        ..Default::default()
    };
    assert_eq!(q.reverse_wildcard_char(), '?', "不看主开关");
    let bad = CodetableGlobal {
        wildcard_key: "12".into(),
        ..Default::default()
    };
    assert_eq!(bad.reverse_wildcard_char(), 'z');
}
