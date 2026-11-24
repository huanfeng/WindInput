//! `engine_reload_needed`（`reload_user_config` 的 `schema_dirty`）的键覆盖。
//!
//! 这组测试守的是一类**很难从现象定位回判据**的缺陷：某个键漏进判据，端到端只表现为
//! 「设置页改了没反应、重启就好」。每加一个「协调器实时读、引擎侧另有镜像」的开关，
//! 这里就该多一条。
use super::*;

/// 临英段变更必须触发引擎重建。
///
/// ★ `input.temp_english.phrase_seg` 有两个消费者：协调器谓词每次按键现读
/// `rt().config`，引擎侧走 `EngineManager::temp_english` 镜像。漏掉这一键时前者立刻
/// 生效、后者永远是旧值 ⇒ `'` 进得了缓冲但引擎 `seg_sep` 仍是 `None` ⇒ 候选塌成
/// 只剩原文。
#[test]
fn temp_english_change_requires_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.input.temp_english.phrase_seg = !old.input.temp_english.phrase_seg;
    assert!(
        engine_reload_needed(&old, &new, wind_config::ConfigOverlay::Base),
        "临英段变更必须让引擎重建，否则引擎侧镜像会停在旧值"
    );
}

/// 移动档下只改 `[mobile.grammar]` 也必须重建；桌面档不看该段，不得重建。
#[test]
fn mobile_grammar_change_follows_overlay() {
    use wind_config::ConfigOverlay::{Base, Mobile};
    let old = Config::default();
    let mut new = old.clone();
    new.mobile.grammar.weight = old.mobile.grammar.weight + 0.1;
    assert!(engine_reload_needed(&old, &new, Mobile));
    assert!(!engine_reload_needed(&old, &new, Base));
}

/// 临拼段同理（既有行为，一并钉住）。
#[test]
fn temp_pinyin_change_requires_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.input.temp_pinyin.enabled = !old.input.temp_pinyin.enabled;
    assert!(engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}

/// schema 段（含 `schema.english.phrase_seg`）同理。
#[test]
fn english_schema_change_requires_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.schema.english.phrase_seg = !old.schema.english.phrase_seg;
    assert!(engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}

/// ★ 反向对照：与引擎无关的段变更**不该**触发重建。
///
/// 没有这条，「恒返回 true」也能让上面三条通过，而那会让每次保存设置都丢词典缓存
/// （拼音合并/unigram 重建开销大，正是判据存在的理由）。
#[test]
fn unrelated_change_does_not_require_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.ui.toolbar.visible = !old.ui.toolbar.visible;
    assert!(
        !engine_reload_needed(&old, &new, wind_config::ConfigOverlay::Base),
        "工具栏可见性与引擎无关，不该触发重建"
    );
}

/// 反查专属的「含扩展词库」开关决定引擎挂不挂影子层，必须触发重建。
#[test]
fn reverse_lookup_switch_requires_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.input.reverse.lookup_disabled_dicts = !old.input.reverse.lookup_disabled_dicts;
    assert!(engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}

/// 反向对照：方案常驻策略由 `apply_residency_change` 就地对齐（摘除 / 后台预热），
/// 改它不该把全部引擎丢掉重建。
#[test]
fn keep_all_loaded_change_does_not_require_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.schema.keep_all_loaded = !old.schema.keep_all_loaded;
    assert!(!engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}

/// 反向对照：词库缓存策略只在启动时读，改它重建引擎也不生效，不该触发重建。
#[test]
fn dict_cache_policy_change_does_not_require_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.schema.dict_cache_policy = "off".to_string();
    assert_ne!(old.schema.dict_cache_policy, new.schema.dict_cache_policy);
    assert!(!engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}

/// 反向对照：反查模式的候选布局只给协调器读，不该丢词典缓存。
#[test]
fn reverse_layout_change_does_not_require_engine_reload() {
    let old = Config::default();
    let mut new = old.clone();
    new.input.reverse.candidate_layout = wind_config::LayoutIntent::Vertical;
    assert!(!engine_reload_needed(
        &old,
        &new,
        wind_config::ConfigOverlay::Base
    ));
}
