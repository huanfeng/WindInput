use super::WildcardDecision::{Enter, Yield};
use crate::coordinator::Coordinator;
use std::path::PathBuf;
use std::sync::Arc;
use wind_config::Config;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 五笔 86 + 通配键 `z`；缺数据返回 `None`（同集成测试的静默跳过约定）。
fn wubi_z(tweak: impl FnOnce(&mut Config)) -> Option<Arc<Coordinator>> {
    if !data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
    {
        eprintln!("跳过：五笔词库不存在");
        return None;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.wildcard = true;
    cfg.schema.codetable.wildcard_key = "z".into();
    tweak(&mut cfg);
    Some(Coordinator::new_headless(cfg, Some(&data_dir())))
}

/// 首位 z 是 `zz*` 活码 ⇒ 首位让位，且**整轮**字面（后续 z 也让位）；
/// 非 z 起头的缓冲里 z 照常作通配。
#[test]
fn leading_literal_wildcard_makes_whole_buffer_literal() {
    let Some(c) = wubi_z(|_| {}) else { return };
    let seed = |code: &str, text: &str| wind_phrase::PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    };
    c.debug_install_phrases(vec![seed("zzbd", "、")]);
    assert_eq!(c.wildcard_decision("", 'z'), Yield, "首位活码让位");
    assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位字面 ⇒ 整轮字面");
    assert_eq!(c.wildcard_decision("zzb", 'z'), Yield);
    assert_eq!(c.wildcard_decision("a", 'z'), Enter, "对照：a 起头照常通配");
}

/// overlay 激活时通配不生效（`wildcard_enters` 的 `state.active` 门）。
/// 现有分派里 overlay 在两个接线点之前就 return，集成层测不到这道门，故在此直接钉。
#[test]
fn overlay_active_blocks_wildcard() {
    let Some(c) = wubi_z(|_| {}) else { return };
    let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
    st.input_buffer = "a".into();
    st.active = None;
    assert!(
        c.wildcard_enters(&st, 'z'),
        "对照：无 overlay 时 a 后的 z 作通配"
    );
    st.active = Some(crate::pipeline::ModeKind::TempPinyin);
    assert!(!c.wildcard_enters(&st, 'z'), "overlay 激活时不作通配");
}

/// `z_key_repeat` 按**配置开关**让位，与有无上屏历史无关（新建协调器无历史）。
/// 按 `z_key_repeat_text()` 判的话这里会得 Enter，同一串缓冲前后解释不一。
#[test]
fn z_key_repeat_yields_by_switch_not_history() {
    let Some(c) = wubi_z(|cfg| cfg.schema.codetable.z_key_repeat = true) else {
        return;
    };
    assert_eq!(c.wildcard_decision("", 'z'), Yield);
    assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位让位 ⇒ 整轮字面");
    assert_eq!(c.wildcard_decision("a", 'z'), Enter);
}

fn seed(code: &str, text: &str) -> wind_phrase::PhraseSeed {
    wind_phrase::PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    }
}

const S: char = wind_dict::WILDCARD_SLOT;

/// 作通配的位换成槽位；无通配键的缓冲不是通配组码。
#[test]
fn pattern_marks_every_wildcard_position() {
    let Some(c) = wubi_z(|_| {}) else { return };
    assert_eq!(c.wildcard_pattern("az"), Some(format!("a{S}")));
    assert_eq!(c.wildcard_pattern("azzd"), Some(format!("a{S}{S}d")));
    assert_eq!(
        c.wildcard_pattern("z"),
        Some(S.to_string()),
        "首位无绑定 ⇒ 首位即通配"
    );
    assert_eq!(c.wildcard_pattern("aaaa"), None);
    assert_eq!(c.wildcard_pattern(""), None);
}

/// 通配关闭 ⇒ 恒 `None`（缓冲里的 `z` 是字面码元）。
#[test]
fn pattern_is_none_when_wildcard_off() {
    let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard = false) else {
        return;
    };
    assert_eq!(c.wildcard_pattern("az"), None);
    assert_eq!(c.wildcard_pattern("z"), None);
}

/// 首位字面（`zz*` 活码让位进来的 `z`）⇒ 整轮字面，与 `wildcard_decision` 同一规则。
#[test]
fn pattern_is_none_when_lead_is_literal() {
    let Some(c) = wubi_z(|_| {}) else { return };
    c.debug_install_phrases(vec![seed("zzbd", "、")]);
    assert_eq!(c.wildcard_pattern("zzbd"), None);
    assert_eq!(c.wildcard_pattern("zz"), None);
    assert_eq!(
        c.wildcard_pattern("az"),
        Some(format!("a{S}")),
        "对照：a 起头照常"
    );
}

/// 非首位有冲突（通配键 `=` 是出厂翻页键）⇒ 非首位的通配键只可能是经让位进来的字面。
/// 首位不受翻页键影响（空缓冲无页可翻）⇒ 照常作通配（论坛 t302）。
#[test]
fn pattern_keeps_mid_positions_literal_on_conflict() {
    let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "=".into()) else {
        return;
    };
    assert_eq!(c.wildcard_decision("", '='), Enter, "首位符号通配键作通配");
    assert_eq!(
        c.wildcard_decision("a", '='),
        Yield,
        "前置：非首位让位给翻页"
    );
    assert_eq!(c.wildcard_pattern("a="), None);
    assert_eq!(
        c.wildcard_pattern("=a="),
        Some(format!("{S}a=")),
        "首位通配、非首位字面"
    );
}

/// 符号通配键首位**作通配**（论坛 t302），与组码中一致；pattern 与按键裁决同一结论。
/// 对照：字母通配键首位全无绑定时同样作通配。
#[test]
fn symbol_wildcard_enters_at_lead() {
    let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "?".into()) else {
        return;
    };
    assert_eq!(c.wildcard_decision("", '?'), Enter);
    assert_eq!(c.wildcard_decision("a", '?'), Enter, "组码中照常作通配");
    assert_eq!(c.wildcard_pattern("a?"), Some(format!("a{S}")));
    assert_eq!(
        c.wildcard_pattern("?a"),
        Some(format!("{S}a")),
        "首位也是通配"
    );

    let Some(c) = wubi_z(|_| {}) else { return };
    assert_eq!(
        c.wildcard_decision("", 'z'),
        Enter,
        "对照：字母键首位无绑定"
    );
}

/// 通配接管全局引导键时要能报出来（出厂反引号 = 临拼触发）；字母键与关闭通配时不报。
#[test]
fn wildcard_lead_override_of_global_binding_is_reported() {
    let Some(c) = wubi_z(|cfg| cfg.schema.codetable.wildcard_key = "`".into()) else {
        return;
    };
    assert!(c.wildcard_lead_overrides_global('`').is_some());
    let Some(c) = wubi_z(|_| {}) else { return };
    assert!(c.wildcard_lead_overrides_global('z').is_none());
}

/// 满码后通配键按字面：`aaaa` + `z` 让位，`aaaaz` 不是通配组码；按键裁决与 pattern
/// 同一判据。对照：未满码时照常通配，满码前的通配位不受影响。
#[test]
fn wildcard_past_full_length_is_literal() {
    let Some(c) = wubi_z(|_| {}) else { return };
    assert_eq!(c.wildcard_decision("aaa", 'z'), Enter, "对照：未满码");
    assert_eq!(c.wildcard_decision("aaaa", 'z'), Yield);
    assert_eq!(c.wildcard_pattern("aaaaz"), None);
    assert_eq!(c.wildcard_pattern("azaaz"), Some(format!("a{S}aaz")));
}

/// ★ Review Focus 3：主输入路的上屏记账对通配组码下的码表候选记**候选全码**，不记
/// `azzz` 这种通配串（读端永远查不中的孤儿键）。`freq_code` 本身不变（mix / 临拼缓冲
/// 与读侧调频沿用它）。对照：非通配组码沿用缓冲；非码表来源不受影响。
#[test]
fn main_freq_code_under_wildcard_is_candidate_full_code() {
    use wind_candidate::{Candidate, CandidateSource};
    let Some(c) = wubi_z(|_| {}) else { return };
    let cand = Candidate {
        text: "工".into(),
        code: "aaaa".into(),
        source: CandidateSource::CodeTable,
        ..Default::default()
    };
    assert_eq!(c.main_freq_code("azzz", &cand), "aaaa");
    assert_eq!(
        c.main_freq_code("aaa", &cand),
        "aaa",
        "对照：非通配沿用缓冲"
    );
    assert_eq!(c.freq_code("azzz", &cand), "azzz", "freq_code 本体不变");
    let phrase = Candidate {
        text: "、".into(),
        source: CandidateSource::Phrase,
        ..Default::default()
    };
    assert_eq!(
        c.main_freq_code("azzz", &phrase),
        c.freq_code("azzz", &phrase)
    );
}

/// 五笔拼音混输 + 通配键 `z`；缺数据返回 `None`。
fn wubi_pinyin_z(tweak: impl FnOnce(&mut Config)) -> Option<Arc<Coordinator>> {
    if !data_dir()
        .join("schemas/wubi86_pinyin.schema.toml")
        .exists()
    {
        eprintln!("跳过：混输方案不存在");
        return None;
    }
    wubi_z(|cfg| {
        cfg.schema.available = vec!["wubi86_pinyin".into(), "wubi86".into(), "pinyin".into()];
        cfg.schema.active = "wubi86_pinyin".into();
        tweak(cfg);
    })
}

/// ★ Review Focus 5：混输首位一律字面（z 是拼音声母），不靠 `zz*` 短语——这里**不装**
/// 短语，混输下 `has_code_prefix("z")` 为假，旧规则会判 Enter。首位字面 ⇒ 整串字面。
#[test]
fn mixed_lead_is_always_literal() {
    let Some(c) = wubi_pinyin_z(|_| {}) else {
        return;
    };
    assert_eq!(c.wildcard_decision("", 'z'), Yield, "混输首位 z 是拼音声母");
    assert_eq!(c.wildcard_decision("z", 'z'), Yield, "首位字面 ⇒ 整轮字面");
    assert_eq!(c.wildcard_pattern("zhan"), None);
    assert_eq!(c.wildcard_pattern("zaz"), None);
    assert_eq!(
        c.wildcard_decision("a", 'z'),
        Enter,
        "对照：非首位码长内照常通配"
    );
}

/// 混输：整串 > 主码表码长 ⇒ 整串字面（连前段通配位一起）；码长取主码表的 4，
/// 不是混输 `max_code_length` 的 0。对照纯五笔 `azaaz` → `a?aaz`（`wildcard_past_full_length_is_literal`）。
#[test]
fn mixed_overlength_buffer_is_literal() {
    let Some(c) = wubi_pinyin_z(|_| {}) else {
        return;
    };
    assert_eq!(c.wildcard_pattern("hanz"), Some(format!("han{S}")));
    assert_eq!(c.wildcard_pattern("gz"), Some(format!("g{S}")));
    assert_eq!(c.wildcard_pattern("hanzi"), None, "超码长整串字面");
    assert_eq!(c.wildcard_pattern("azaaz"), None, "前段通配位也随整串字面");
    assert_eq!(c.wildcard_decision("xian", 'z'), Yield, "码长取主码表的 4");
    assert_eq!(c.wildcard_decision("han", 'z'), Enter);
}

/// ★ Review Focus 4：拼音分段续转态（最后一段是拼音选词）剩余串一律字面，按键也不作通配。
#[test]
fn pinyin_continuation_is_literal() {
    let Some(c) = wubi_pinyin_z(|_| {}) else {
        return;
    };
    let mut st = crate::coordinator::State {
        input_buffer: "aizi".into(),
        ..Default::default()
    };
    assert_eq!(
        c.wildcard_pattern_of(&st),
        Some(format!("ai{S}i")),
        "前置：无分段时码长内照常通配"
    );
    st.committed_segs.push(crate::coordinator::CommittedSeg {
        raw_code: "wo".into(),
        code: "wo".into(),
        text: "我".into(),
        source: wind_candidate::CandidateSource::Pinyin,
        boundary: 0,
        learn: None,
    });
    assert_eq!(c.wildcard_pattern_of(&st), None, "拼音分段续转 ⇒ 字面");
    st.input_buffer = "ai".into();
    assert!(!c.wildcard_enters(&st, 'z'), "续转态按键也不作通配");
    st.committed_segs.clear();
    assert!(c.wildcard_enters(&st, 'z'), "对照：无分段时作通配");
}
