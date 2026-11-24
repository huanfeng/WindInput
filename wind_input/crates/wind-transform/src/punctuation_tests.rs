use super::*;

#[test]
fn test_basic_punct() {
    let mut p = PunctuationConverter::new();
    assert_eq!(p.to_chinese(',').as_deref(), Some("，"));
    assert_eq!(p.to_chinese('.').as_deref(), Some("。"));
    assert_eq!(p.to_chinese('\\').as_deref(), Some("、"));
    assert_eq!(p.to_chinese('^').as_deref(), Some("……"));
    assert_eq!(p.to_chinese('a'), None);
}

/// 测试用自定义映射配置：'/' 四列全配 + 双引号左右分键（仅中文半角列）。
fn custom_cfg(enabled: bool) -> PunctConfig {
    let mut c = PunctConfig {
        custom_enabled: enabled,
        ..PunctConfig::default()
    };
    // '/' 自定义：中半=、 英全=／ 中全=、 英半=/
    c.custom_mappings.insert(
        "/".to_string(),
        vec!["、".into(), "／".into(), "、".into(), "/".into()],
    );
    c.custom_mappings
        .insert("\"1".to_string(), vec!["「".into()]);
    c.custom_mappings
        .insert("\"2".to_string(), vec!["」".into()]);
    c
}

#[test]
fn test_custom_mapping() {
    let mut p = PunctuationConverter::new();
    let cfg = custom_cfg(true);
    assert_eq!(p.lookup_custom(&cfg, '/', 0).as_deref(), Some("、")); // 中文半角
    assert_eq!(p.lookup_custom(&cfg, '/', 1).as_deref(), Some("／")); // 英文全角
    assert_eq!(p.lookup_custom(&cfg, '/', 3).as_deref(), Some("/")); // 英文半角
    assert_eq!(p.lookup_custom(&cfg, 'a', 0), None); // 无映射
    // 引号按左右交替选 key 并切换状态
    assert_eq!(p.lookup_custom(&cfg, '"', 0).as_deref(), Some("「")); // 左
    assert_eq!(p.lookup_custom(&cfg, '"', 0).as_deref(), Some("」")); // 右
}

#[test]
fn test_custom_disabled() {
    let mut p = PunctuationConverter::new();
    assert_eq!(p.lookup_custom(&custom_cfg(false), '/', 0), None); // 未启用
}

/// 配置是**参数**而非转换器内的副本：同一实例，配置换了下一次查表即跟随
/// （回归锁：曾把映射表存进转换器且只在启动时注入一次，热重载后仍用旧表）。
#[test]
fn custom_mapping_follows_live_config() {
    let mut p = PunctuationConverter::new();
    assert_eq!(p.lookup_custom(&PunctConfig::default(), '/', 0), None); // 出厂无映射
    assert_eq!(
        p.lookup_custom(&custom_cfg(true), '/', 0).as_deref(),
        Some("、")
    );
}

/// peek 不推进引号态，且引号也走 `"1`/`"2` 键（曾按 `"` 拼键 → 引号永远查不到自定义）。
#[test]
fn peek_custom_quote_key_without_advancing() {
    let p = PunctuationConverter::new();
    let cfg = custom_cfg(true);
    assert_eq!(p.custom_key('"'), "\"1");
    assert_eq!(p.peek_custom(&cfg, '"', 0).as_deref(), Some("「"));
    assert_eq!(p.peek_custom(&cfg, '"', 0).as_deref(), Some("「")); // 未推进
}

#[test]
fn test_peek_and_revert() {
    let mut p = PunctuationConverter::new();
    // peek 不改状态：连续 peek 同引号返回相同（左）
    assert_eq!(p.peek_chinese_str('"').as_deref(), Some("\u{201C}"));
    assert_eq!(p.peek_chinese_str('"').as_deref(), Some("\u{201C}"));
    assert_eq!(p.peek_chinese_str('.').as_deref(), Some("。"));
    // to_chinese 推进到右，revert 退回左
    assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // 左→推进
    p.revert_last_quote('"');
    assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // revert 后仍为左
}

#[test]
fn test_quote_toggle() {
    let mut p = PunctuationConverter::new();
    assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // 左
    assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201D}")); // 右
    assert_eq!(p.to_chinese('\'').as_deref(), Some("\u{2018}"));
    p.reset();
    assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // reset 后回到左
}
