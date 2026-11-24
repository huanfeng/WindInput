// DEFAULT_EMOJI_FAMILY 只被下面 `#[cfg(windows)]` 的出厂指派用例引用，
// 导入不带 cfg 的话 macOS/Linux 上就是 unused_imports（CI 的 -D warnings 会红）。
#[cfg(windows)]
use super::DEFAULT_EMOJI_FAMILY;
use super::{DEFAULT_FONT_FAMILY, build_font_plan};
use crate::text::script::ScriptClass;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// ★ 空字族的链首必须是内置默认，不能是空串。
///
/// 空串链首的表现最阴：`AddMapping` 的 `baseFamilyName` 是空串就永远匹配不上任何段
/// ⇒「用户没显式写 family 时，配了 fallback 也不生效」，而配置、日志、界面全都正常。
#[test]
fn empty_family_becomes_the_builtin_default_at_the_chain_head() {
    let p = build_font_plan("", &v(&["Noto Sans Mongolian"]), &[]);
    assert_eq!(p.base_family(), Some(DEFAULT_FONT_FAMILY));
    assert_eq!(
        p.chain_for(None),
        [DEFAULT_FONT_FAMILY, "Noto Sans Mongolian"]
    );
    // 只有空白也算空。
    assert_eq!(
        build_font_plan("   ", &[], &[]).base_family(),
        Some(DEFAULT_FONT_FAMILY)
    );
}

/// 默认链 = `[family] + fallback`，顺序不能颠倒（颠倒后主字体成了回退项）。
#[test]
fn default_chain_is_family_then_fallback() {
    let p = build_font_plan(
        "Mongolian Baiti",
        &v(&["Noto Sans Mongolian", "Segoe UI"]),
        &[],
    );
    assert_eq!(
        p.chain_for(None),
        ["Mongolian Baiti", "Noto Sans Mongolian", "Segoe UI"]
    );
}

/// 已知脚本类名进方案，未知的**只忽略这一条**——不得连累同一份配置里其余的类。
/// 用户拼错一个键就让整份字体配置失效是不能接受的。
#[test]
fn unknown_script_key_is_dropped_without_affecting_the_others() {
    let p = build_font_plan(
        "Mongolian Baiti",
        &[],
        &[
            ("latin".to_string(), v(&["Segoe UI"])),
            ("mongolian".to_string(), v(&["Whatever"])), // 未知：不是具名类
            ("CJK".to_string(), v(&["宋体"])),           // 大小写不敏感
        ],
    );
    // 顺序是 `ScriptClass` 的**声明序**（`FontPlan::new` 按 `Ord` 排），不是字母序。
    // Windows 上出厂会再注入 emoji 一项，这里只看用户声明的那两条。
    let user_declared: Vec<ScriptClass> = p
        .declared()
        .iter()
        .copied()
        .filter(|c| *c != ScriptClass::Emoji)
        .collect();
    assert_eq!(user_declared, [ScriptClass::Latin, ScriptClass::Cjk]);
    assert_eq!(p.chain_for(Some(ScriptClass::Latin)), ["Segoe UI"]);
    assert_eq!(p.chain_for(Some(ScriptClass::Cjk)), ["宋体"]);
}

/// 零配置（出厂）必须折成平凡方案——调用方据此完全走旧路径，一次 COM 调用都不多做。
///
/// Windows 除外：出厂会注入 emoji 类的指派（见 `build_font_plan`），零配置不再平凡，
/// 由 `factory_config_assigns_emoji_on_windows` 守。
#[cfg(not(windows))]
#[test]
fn factory_config_folds_to_a_trivial_plan() {
    assert!(build_font_plan("", &[], &[]).is_trivial());
    assert!(build_font_plan("宋体", &[], &[]).is_trivial());
}

/// 反向对照（各平台）：只要有一项非零配置就不再平凡（否则「平凡」判据形同虚设）。
#[test]
fn any_non_factory_item_makes_the_plan_non_trivial() {
    assert!(!build_font_plan("宋体", &v(&["Arial"]), &[]).is_trivial());
    assert!(!build_font_plan("宋体", &[], &[("latin".to_string(), v(&["Arial"]))]).is_trivial());
}

/// ★ Windows 出厂：emoji 类恒有指派（Segoe UI Emoji），否则 1️⃣ 与 Emoji 16.0 的新码位
/// 渲染不出来；用户显式声明 emoji（含空表）时按用户的来，空表即关掉这条指派。
#[cfg(windows)]
#[test]
fn factory_config_assigns_emoji_on_windows() {
    let p = build_font_plan("", &[], &[]);
    assert_eq!(p.declared(), &[ScriptClass::Emoji]);
    assert_eq!(
        p.chain_for(Some(ScriptClass::Emoji)),
        [DEFAULT_EMOJI_FAMILY]
    );
    // 其余零配置语义不变：默认链仍只有一项。
    assert_eq!(p.chain_for(None), [DEFAULT_FONT_FAMILY]);

    // 用户自己指派 emoji：不覆盖。
    let p = build_font_plan("", &[], &[("emoji".to_string(), v(&["Twemoji"]))]);
    assert_eq!(p.chain_for(Some(ScriptClass::Emoji)), ["Twemoji"]);

    // 逃生口：`emoji = []` 关掉出厂指派，回到纯系统回退。
    let p = build_font_plan("", &[], &[("emoji".to_string(), vec![])]);
    assert!(
        p.is_trivial(),
        "emoji = [] 应关掉出厂指派：{:?}",
        p.declared()
    );
}
