//! `input.rare_phrase` 的**跨 crate 契约**：配置默认值、注册表值域、判据枚举三处一致。
//!
//! # 为什么在 coordinator
//!
//! 判据枚举 `wind_candidate::RarePhrasePolicy` 与配置 `wind_config` 互不依赖（wind-candidate
//! 不认得配置、wind-config 不认得判据），只有这里同时看得见两边。同形的 `NewlineStyle`
//! 能把对照测试写在 wind-config 内，是因为那个枚举本就住在 wind-config 里。
//!
//! 三处漂移的症状都是**静默**的：设置页写进一个 core 不认的值 ⇒ 落回默认而界面照常显示
//! 用户选的那项；或者出厂默认与枚举默认分叉 ⇒ 「没配」与「配错」表现不一致。

use wind_candidate::RarePhrasePolicy;
use wind_config::config_schema::{self, FieldType};

/// 出厂默认与判据默认必须是同一档。
#[test]
fn config_default_matches_policy_default() {
    let cfg = wind_config::config::InputConfig::default();
    assert_eq!(
        RarePhrasePolicy::from_config(&cfg.rare_phrase),
        RarePhrasePolicy::default(),
        "InputConfig 的出厂值 {:?} 解析后与 RarePhrasePolicy::default() 不是同一档",
        cfg.rare_phrase
    );
    assert_eq!(
        cfg.rare_phrase,
        RarePhrasePolicy::default().as_config(),
        "两处默认值的**字面量**也要一致，否则设置页与配置文件里显示的默认项对不上"
    );
}

/// 注册表登记的值域 == 枚举的全部变体（双向，一个都不能多、不能少）。
#[test]
fn registry_values_match_the_policy_enum() {
    let field = config_schema::field("input.rare_phrase").expect("注册表里必须登记这个键");
    let FieldType::Enum(values) = field.ty else {
        panic!(
            "须登记为 Enum（值域进注册表，设置端才比得了），实为 {:?}",
            field.ty
        );
    };
    // 注册表 → 枚举：登记的每个值都要认得。认不得的值会被 `from_config` 静默落回 Keep。
    for v in values {
        assert_eq!(
            RarePhrasePolicy::from_config(v).as_config(),
            *v,
            "注册表登记了 {v:?}，但 RarePhrasePolicy 不认得它"
        );
    }
    // 枚举 → 注册表：新增变体后注册表不能漏登记，否则校验会把合法值判成越界。
    for p in [RarePhrasePolicy::Keep, RarePhrasePolicy::Filter] {
        assert!(
            values.contains(&p.as_config()),
            "枚举有 {p:?} 而注册表值域 {values:?} 没登记"
        );
    }
}
