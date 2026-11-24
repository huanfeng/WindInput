use super::*;

/// 预置后缀与学习数据必须同域：两边都存 `@` 之后的部分，否则去重对不上，
/// 同一后缀在候选里出现两次。
#[test]
fn preset_suffixes_carry_no_at_sign() {
    let c = EmailConfig::default();
    assert!(!c.suffixes.is_empty(), "出厂应带预置后缀");
    for s in &c.suffixes {
        assert!(!s.contains('@'), "预置后缀存 @ 之后的部分，{s:?} 不该带 @");
        assert!(s.contains('.'), "后缀应是域名形态，{s:?} 不像");
    }
}

/// 预置表不得有重复项——重复项会在候选里变成两条一模一样的邮箱。
#[test]
fn preset_suffixes_are_unique() {
    let c = EmailConfig::default();
    let mut seen = std::collections::HashSet::new();
    for s in &c.suffixes {
        assert!(seen.insert(s.as_str()), "预置后缀重复：{s}");
    }
}

/// 国内常用在前：冷启动（还没有任何学习数据）时排序完全由这份顺序决定。
#[test]
fn domestic_suffixes_come_first() {
    let c = EmailConfig::default();
    let pos = |needle: &str| c.suffixes.iter().position(|s| s == needle);
    let qq = pos("qq.com").expect("应含 qq.com");
    let gmail = pos("gmail.com").expect("应含 gmail.com");
    assert!(qq < gmail, "国内常用后缀应排在国际之前");
}

/// 用户清空后缀表 = 真的空表（AGENTS.md「出厂预置的落点必须是用户能清空的载体」）。
/// `suffixes` 是数组字段，上层整体覆盖，不会像 map 那样被深合并把出厂值合回来。
/// 空表时邮箱模式仍可用，只是没有预置候选。
#[test]
fn empty_suffix_list_survives_deserialization() {
    let c: EmailConfig = toml::from_str("enabled = true\nsuffixes = []\n").unwrap();
    assert!(c.enabled);
    assert!(c.suffixes.is_empty(), "用户清空后不该被出厂值合回来");
}

/// 缺字段时逐个落到默认值（老配置文件升级上来不会炸）。
#[test]
fn missing_fields_fall_back_to_defaults() {
    let c: EmailConfig = toml::from_str("").unwrap();
    assert!(!c.enabled);
    assert_eq!(c.suffixes, default_email_suffixes());

    let u: UrlConfig = toml::from_str("enabled = true\n").unwrap();
    assert!(u.enabled);
    assert!(!u.history_enabled, "老配置没有 history_enabled ⇒ 关闭");
    assert_eq!(u.history_max, 200);
}
